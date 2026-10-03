//! Shared helpers for the browser suites: drive `owallet_web::handle` the
//! way the JS host does, with real `Request`/`Response` objects.
#![allow(dead_code)]

use js_sys::Uint8Array;
use serde_json::{json, Value};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

/// The mock Overpay (`tests/mock-overpay/server.mjs`), started by the
/// harness before the browser runs; the URL is baked in at build time.
pub fn mock_url() -> &'static str {
    option_env!("MOCK_OVERPAY_URL").unwrap_or("http://127.0.0.1:4010")
}

pub async fn init(storage: &str, db_name: &str) {
    let config = json!({
        "rails_url": mock_url(),
        "env": "dev",
        "storage": storage,
        "db_name": db_name,
        "opfs_directory": ".owallet-web-test",
    });
    // A plain JS object, as the TypeScript host passes it.
    let config = js_sys::JSON::parse(&config.to_string()).unwrap();
    owallet_web::init(config).await.expect("init");
}

pub async fn fetch(
    method: &str,
    path: &str,
    key: Option<&str>,
    body: Option<Value>,
) -> web_sys::Response {
    let init = web_sys::RequestInit::new();
    init.set_method(method);
    let headers = web_sys::Headers::new().unwrap();
    headers
        .set("accept", "application/json, text/event-stream")
        .unwrap();
    if let Some(k) = key {
        headers
            .set("authorization", &format!("Bearer {k}"))
            .unwrap();
    }
    if let Some(b) = body {
        headers.set("content-type", "application/json").unwrap();
        init.set_body(&JsValue::from_str(&b.to_string()));
    }
    init.set_headers(&headers);
    let req =
        web_sys::Request::new_with_str_and_init(&format!("http://owallet.internal{path}"), &init)
            .unwrap();
    JsFuture::from(owallet_web::handle(req))
        .await
        .expect("handle")
        .unchecked_into()
}

pub async fn text(resp: &web_sys::Response) -> String {
    JsFuture::from(resp.text().unwrap())
        .await
        .unwrap()
        .as_string()
        .unwrap()
}

pub async fn json_call(
    method: &str,
    path: &str,
    key: Option<&str>,
    body: Option<Value>,
) -> (u16, Value) {
    let resp = fetch(method, path, key, body).await;
    let status = resp.status();
    let t = text(&resp).await;
    (
        status,
        serde_json::from_str(&t).unwrap_or_else(|_| panic!("{path}: not JSON: {t}")),
    )
}

/// Read a streamed body chunk by chunk, as a client would.
pub async fn read_chunks(resp: &web_sys::Response) -> Vec<String> {
    let reader: web_sys::ReadableStreamDefaultReader = resp
        .body()
        .expect("streamed body")
        .get_reader()
        .unchecked_into();
    let mut chunks = Vec::new();
    loop {
        let result = JsFuture::from(reader.read()).await.unwrap();
        let done = js_sys::Reflect::get(&result, &"done".into())
            .unwrap()
            .as_bool()
            .unwrap_or(true);
        if done {
            break;
        }
        let value = js_sys::Reflect::get(&result, &"value".into()).unwrap();
        chunks.push(String::from_utf8(Uint8Array::new(&value).to_vec()).unwrap());
    }
    chunks
}

/// init → generate → provider-key/create; returns the key.
pub async fn bootstrap() -> String {
    let (s, v) = json_call("POST", "/_mgmt/init", None, Some(json!({"password": "pw"}))).await;
    assert_eq!(s, 200, "{v}");
    let (s, v) = json_call("POST", "/_mgmt/generate", None, None).await;
    assert_eq!(s, 200, "{v}");
    assert!(v["npub"].as_str().unwrap().starts_with("npub1"));
    let (s, v) = json_call(
        "POST",
        "/_mgmt/provider-key/create",
        None,
        Some(json!({"label": "norm", "spend": true, "budget_usd": 5})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["scopes"], "chat spend");
    assert_eq!(v["daily_budget_usd_cents"], 500);
    v["key"].as_str().unwrap().to_string()
}
