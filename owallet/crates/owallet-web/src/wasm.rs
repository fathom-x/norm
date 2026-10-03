//! The JS face of owallet-web (wasm32-unknown-unknown only).
//!
//! ```js
//! import init_wasm, { init, handle } from "./owallet_web.js";
//! await init_wasm();
//! await init({ rails_url: "https://overpay.com", env: "prod", storage: "opfs" });
//! const res = await handle(new Request("http://owallet.internal/health"));
//! ```
//!
//! `handle` answers any `Request` through the router. Response bodies are
//! a `ReadableStream` pulled frame by frame from the Rust body, so SSE
//! (`/v1/chat/completions` with `stream: true`, `/mcp` `tools/call`) arrives
//! as it is produced; cancelling the JS stream drops the Rust one, which
//! drops the in-flight work.

use std::cell::RefCell;
use std::path::PathBuf;

use axum::body::Body;
use axum::Router;
use futures_util::StreamExt;
use js_sys::{Array, Uint8Array};
use tower::ServiceExt;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

use crate::{App, MgmtError, Storage, WebConfig};

#[wasm_bindgen(typescript_custom_section)]
const TS_CONFIG: &str = r#"
/** `init(config)` — the whole browser-side configuration (no env vars). */
export interface OwalletWebConfig {
  /** Overpay API base URL; optional only for env "prod". */
  rails_url?: string;
  /** Browser-facing Overpay URL (defaults to rails_url). */
  public_url?: string;
  /** "prod" | "dev" | "staging" (default "prod"); reported by /_mgmt/status. */
  env?: string;
  /** "opfs" (dedicated worker only; persists) or "memory" (default). */
  storage?: "opfs" | "memory";
  /** Database file name in the VFS (default "owallet.db"). */
  db_name?: string;
  /** OPFS directory of the access-handle pool (default ".owallet-web"). */
  opfs_directory?: string;
}
"#;

thread_local! {
    static ROUTER: RefCell<Option<Router>> = const { RefCell::new(None) };
}

#[wasm_bindgen(start)]
fn start() {
    console_error_panic_hook::set_once();
}

/// `init(config)`: install the storage VFS and build the router. Resolves
/// to nothing; rejects with an `Error` naming the problem. Calling it again
/// replaces the configuration (the database is reopened on next use).
#[wasm_bindgen]
pub async fn init(
    #[wasm_bindgen(unchecked_param_type = "OwalletWebConfig")] config: JsValue,
) -> Result<(), JsValue> {
    let config: WebConfig = serde_wasm_bindgen::from_value(config)
        .map_err(|e| js_error(&format!("owallet-web config: {e}")))?;
    let db_name = config
        .db_name
        .clone()
        .unwrap_or_else(|| "owallet.db".into());
    match config.storage {
        Storage::Opfs => install_opfs(config.opfs_directory.as_deref()).await?,
        // sqlite-wasm-rs' memory VFS is SQLite's default VFS.
        Storage::Memory => {}
    }
    let app = App::from_config(&config, PathBuf::from(format!("/{db_name}")))
        .map_err(|e| js_error(&e.to_string()))?;
    ROUTER.with(|r| *r.borrow_mut() = Some(crate::router(app)));
    Ok(())
}

/// The OPFS sync-access-handle pool, registered as SQLite's default VFS.
/// Installed once per page; needs a dedicated worker.
async fn install_opfs(directory: Option<&str>) -> Result<(), JsValue> {
    thread_local! {
        static INSTALLED: RefCell<bool> = const { RefCell::new(false) };
    }
    if INSTALLED.with(|i| *i.borrow()) {
        return Ok(());
    }
    use sqlite_wasm_vfs::sahpool::{install, OpfsSAHPoolCfgBuilder};
    let cfg = OpfsSAHPoolCfgBuilder::new()
        .vfs_name("owallet-opfs")
        .directory(directory.unwrap_or(".owallet-web"))
        .build();
    install(&cfg, true)
        .await
        .map_err(|e| js_error(&format!("OPFS storage (dedicated worker required): {e}")))?;
    INSTALLED.with(|i| *i.borrow_mut() = true);
    Ok(())
}

/// Load a database file's bytes into the in-memory VFS under `name` (the
/// file `init({storage: "memory", db_name: name})` opens). For tests and
/// for restoring an exported wallet.
#[wasm_bindgen(js_name = importMemoryDb)]
pub fn import_memory_db(name: &str, bytes: &[u8]) -> Result<(), JsValue> {
    sqlite_wasm_rs::MemVfsUtil::new()
        .import_db(&format!("/{name}"), bytes)
        .map_err(|e| js_error(&format!("import: {e}")))
}

/// `handle(request) -> Promise<Response>`: answer one fetch-shaped request.
#[wasm_bindgen(unchecked_return_type = "Promise<Response>")]
pub fn handle(req: web_sys::Request) -> js_sys::Promise {
    wasm_bindgen_futures::future_to_promise(async move {
        let resp = match ROUTER.with(|r| r.borrow().clone()) {
            Some(router) => {
                let req = to_http(&req).await?;
                match router.oneshot(req).await {
                    Ok(resp) => resp,
                    Err(never) => match never {},
                }
            }
            None => axum::response::IntoResponse::into_response(MgmtError::new(
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "not_initialized",
                "owallet-web: call init(config) first",
            )),
        };
        to_web(resp).map(JsValue::from)
    })
}

/// `web_sys::Request` → `http::Request` (buffered body; only the path and
/// query of the URL matter).
async fn to_http(req: &web_sys::Request) -> Result<http::Request<Body>, JsValue> {
    let url = web_sys_url_path(&req.url());
    let mut builder = http::Request::builder()
        .method(req.method().as_str())
        .uri(url);
    let entries = js_sys::try_iter(&req.headers())?
        .ok_or_else(|| js_error("request headers are not iterable"))?;
    for entry in entries {
        let pair: Array = entry?.unchecked_into();
        if let (Some(k), Some(v)) = (pair.get(0).as_string(), pair.get(1).as_string()) {
            builder = builder.header(k, v);
        }
    }
    let body = match req.method().as_str() {
        "GET" | "HEAD" => Vec::new(),
        _ => {
            let buf = JsFuture::from(req.array_buffer()?).await?;
            Uint8Array::new(&buf).to_vec()
        }
    };
    builder
        .body(Body::from(body))
        .map_err(|e| js_error(&format!("bad request: {e}")))
}

/// The path-and-query part of an absolute URL.
fn web_sys_url_path(url: &str) -> String {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    match after_scheme.find('/') {
        Some(i) => after_scheme[i..].to_string(),
        None => "/".to_string(),
    }
}

/// `http::Response` → `web_sys::Response` with a streamed body.
fn to_web(resp: http::Response<Body>) -> Result<web_sys::Response, JsValue> {
    let (parts, body) = resp.into_parts();
    let headers = web_sys::Headers::new()?;
    for (k, v) in &parts.headers {
        if let Ok(v) = v.to_str() {
            headers.append(k.as_str(), v)?;
        }
    }
    let init = web_sys::ResponseInit::new();
    init.set_status(parts.status.as_u16());
    init.set_headers(&headers);
    let null_body = matches!(parts.status.as_u16(), 101 | 204 | 205 | 304);
    if null_body {
        return web_sys::Response::new_with_opt_str_and_init(None, &init);
    }
    let frames = body.into_data_stream().map(|chunk| match chunk {
        Ok(bytes) => Ok(JsValue::from(Uint8Array::from(bytes.as_ref()))),
        Err(e) => Err(js_error(&format!("response body: {e}"))),
    });
    let stream = wasm_streams::ReadableStream::from_stream(frames).into_raw();
    web_sys::Response::new_with_opt_readable_stream_and_init(Some(&stream), &init)
}

fn js_error(msg: &str) -> JsValue {
    js_sys::Error::new(msg).into()
}
