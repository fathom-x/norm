//! The owallet-web router driven natively (same code the wasm build runs,
//! minus the JS glue) against the Node mock Overpay in `mock-overpay/`.
//! Needs `node` on PATH; the browser suite (`tests/browser.rs`) covers the
//! wasm side.
#![cfg(not(all(target_family = "wasm", target_os = "unknown")))]

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

use axum::body::Body;
use axum::Router;
use http::{Request, StatusCode};
use http_body_util::BodyExt;
use owallet_web::{router, App, WebConfig};
use serde_json::{json, Value};
use tower::ServiceExt;

struct Mock {
    child: Child,
    url: String,
}

impl Drop for Mock {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn start_mock() -> Mock {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/mock-overpay/server.mjs");
    let mut child = Command::new("node")
        .arg(script)
        .env("PORT", "0")
        .env("MOCK_STREAM_POLLS", "2")
        .stdout(Stdio::piped())
        .spawn()
        .expect("node on PATH");
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let url = line
        .trim()
        .rsplit(' ')
        .next()
        .expect("listening line")
        .to_string();
    Mock { child, url }
}

fn app(mock: &Mock, dir: &tempfile::TempDir) -> Router {
    let config: WebConfig = serde_json::from_value(json!({
        "rails_url": mock.url, "env": "dev", "storage": "memory",
    }))
    .unwrap();
    router(App::from_config(&config, dir.path().join("owallet.db")).unwrap())
}

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    auth: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, String) {
    let mut req = Request::builder().method(method).uri(path);
    if let Some(key) = auth {
        req = req.header("authorization", format!("Bearer {key}"));
    }
    if body.is_some() {
        req = req.header("content-type", "application/json");
    }
    req = req.header("accept", "application/json, text/event-stream");
    let req = req
        .body(Body::from(body.map(|b| b.to_string()).unwrap_or_default()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn json_call(
    app: &Router,
    method: &str,
    path: &str,
    auth: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let (status, text) = call(app, method, path, auth, body).await;
    (
        status,
        serde_json::from_str(&text).unwrap_or_else(|_| panic!("{path}: not JSON: {text}")),
    )
}

/// init → generate → provider-key/create; returns the key.
async fn bootstrap(app: &Router) -> String {
    let (s, v) = json_call(
        app,
        "POST",
        "/_mgmt/init",
        None,
        Some(json!({"password": "pw"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = json_call(app, "POST", "/_mgmt/generate", None, None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(v["npub"].as_str().unwrap().starts_with("npub1"));
    assert!(v.get("mnemonic").is_none());
    let (s, v) = json_call(
        app,
        "POST",
        "/_mgmt/provider-key/create",
        None,
        Some(json!({"label": "norm", "spend": true, "budget_usd": 5})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
    assert_eq!(
        keys,
        [
            "daily_budget_usd_cents",
            "id",
            "key",
            "label",
            "npub",
            "scopes"
        ]
    );
    assert_eq!(v["scopes"], "chat spend");
    assert_eq!(v["daily_budget_usd_cents"], 500);
    v["key"].as_str().unwrap().to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn health_and_status_before_init() {
    let mock = start_mock();
    let dir = tempfile::tempdir().unwrap();
    let app = app(&mock, &dir);
    let (s, v) = json_call(&app, "GET", "/health", None, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["name"], "owallet");
    assert_eq!(v["version"], owallet_mcp::VERSION);

    let (_, v) = json_call(&app, "GET", "/_mgmt/status", None, None).await;
    assert_eq!(v["initialized"], false);
    assert_eq!(v["unlocked"], false);
    assert_eq!(v["wallet"], Value::Null);
    assert_eq!(v["env"], "dev");

    let (s, v) = json_call(&app, "GET", "/v1/models", Some("owk_x"), None).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(v["error"]["code"], "not_initialized");
    let (s, v) = json_call(
        &app,
        "POST",
        "/_mgmt/provider-key/create",
        None,
        Some(json!({})),
    )
    .await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{v}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mgmt_lifecycle_and_v1() {
    let mock = start_mock();
    let dir = tempfile::tempdir().unwrap();
    let app = app(&mock, &dir);
    let key = bootstrap(&app).await;

    let (s, v) = json_call(
        &app,
        "POST",
        "/_mgmt/init",
        None,
        Some(json!({"password": "x"})),
    )
    .await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::CONFLICT, Some("already_initialized"))
    );

    // A fresh router over the same file: locked until unlocked.
    let reopened = self::app(&mock, &dir);
    let (_, v) = json_call(&reopened, "GET", "/_mgmt/status", None, None).await;
    assert_eq!(
        (v["initialized"].as_bool(), v["unlocked"].as_bool()),
        (Some(true), Some(false))
    );
    let (s, v) = json_call(
        &reopened,
        "POST",
        "/_mgmt/provider-key/create",
        None,
        Some(json!({})),
    )
    .await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::CONFLICT, Some("locked"))
    );
    let (s, v) = json_call(
        &reopened,
        "POST",
        "/_mgmt/unlock",
        None,
        Some(json!({"password": "nope"})),
    )
    .await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("bad_password"))
    );
    let (s, v) = json_call(
        &reopened,
        "POST",
        "/_mgmt/unlock",
        None,
        Some(json!({"password": "pw"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["unlocked"], true);
    assert!(v["wallet"]["npub"].as_str().unwrap().starts_with("npub1"));
    assert_eq!(v["overpay_linked"], false);

    // /v1/status with the minted key (NIP-98 fallback against the mock).
    let (s, v) = json_call(&app, "GET", "/v1/status", Some(&key), None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["key_can_spend"], true);
    assert!(v["merchant_credits"].as_array().is_some(), "{v}");
    assert!(
        v["balance_error"]
            .as_str()
            .unwrap()
            .starts_with("unavailable_in_browser"),
        "{v}"
    );

    let (s, v) = json_call(&app, "GET", "/v1/models", Some(&key), None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let ids: Vec<_> = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids, ["default", "mock/chat-small", "mock/chat-large"]);

    // Link to Overpay: zero-click NIP-98 sign-up, then PKCE.
    let (s, v) = json_call(&app, "POST", "/_mgmt/overpay/register", None, None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["linked"], true);
    assert_eq!(v["username"], "mock-buyer");
    let (_, v) = json_call(&app, "GET", "/_mgmt/status", None, None).await;
    assert_eq!(v["overpay_linked"], true);
    let (s, v) = json_call(
        &app,
        "POST",
        "/_mgmt/overpay/pkce/start",
        None,
        Some(json!({"redirect_uri": "https://norm.example/oauth/callback.html"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(v["authorize_url"]
        .as_str()
        .unwrap()
        .contains("code_challenge="));
    let state = v["state"].as_str().unwrap().to_string();
    let (s, v) = json_call(
        &app,
        "POST",
        "/_mgmt/overpay/pkce/finish",
        None,
        Some(json!({"code": "mock-code", "state": state})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["linked"], true);
    let (s, v) = json_call(
        &app,
        "POST",
        "/_mgmt/overpay/pkce/finish",
        None,
        Some(json!({"code": "mock-code", "state": "stale"})),
    )
    .await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("unknown_state"))
    );

    let (s, v) = json_call(&app, "GET", "/_mgmt/credits", None, None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["data"][0]["core"], true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chat_completions_streamed_and_buffered() {
    let mock = start_mock();
    let dir = tempfile::tempdir().unwrap();
    let app = app(&mock, &dir);
    let key = bootstrap(&app).await;
    let msg = json!([{"role": "user", "content": "hi"}]);

    let (s, text) = call(
        &app,
        "POST",
        "/v1/chat/completions",
        Some(&key),
        Some(json!({"model": "mock/chat-small", "stream": true, "messages": msg})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{text}");
    assert!(text.trim_end().ends_with("data: [DONE]"), "{text}");
    let events: Vec<Value> = text
        .lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .filter(|d| *d != "[DONE]")
        .map(|d| serde_json::from_str(d).unwrap())
        .collect();
    let content: String = events
        .iter()
        .filter_map(|e| {
            e.pointer("/choices/0/delta/content")
                .and_then(Value::as_str)
        })
        .collect();
    assert!(
        content.contains("Hello from the mock seller."),
        "{content:?}"
    );
    let usage = events
        .iter()
        .find_map(|e| e.get("usage").filter(|u| !u.is_null()))
        .expect("usage");
    // mock/chat-small is metered: one create+pay request with a buyer-set
    // authorization, then the seller's 3¢ capture.
    assert_eq!(usage["charged_cents"], 3, "{usage}");

    let (s, v) = json_call(
        &app,
        "POST",
        "/v1/chat/completions",
        Some(&key),
        Some(json!({"model": "default", "messages": msg})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        v["choices"][0]["message"]["content"],
        "Hello from the mock seller."
    );
    // `default` matches no variant, so this turn took the plain path
    // (create, then redeem the 1¢ listing price): the 3¢ the seller states
    // is clamped to what was paid.
    assert_eq!(v["usage"]["charged_cents"], 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_tools_list_and_call() {
    let mock = start_mock();
    let dir = tempfile::tempdir().unwrap();
    let app = app(&mock, &dir);
    let key = bootstrap(&app).await;
    let rpc = |id: u32, method: &str, params: Value| json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});

    let (s, v) = json_call(
        &app,
        "POST",
        "/mcp",
        Some(&key),
        Some(rpc(1, "tools/list", json!({}))),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let names: Vec<_> = v["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"get_account_info") && names.contains(&"run_python"),
        "{names:?}"
    );

    let (s, v) = json_call(
        &app,
        "POST",
        "/mcp",
        Some("owk_bogus"),
        Some(rpc(2, "tools/list", json!({}))),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{v}");

    // No anonymous /mcp in the browser: a missing bearer is refused.
    let (s, v) = json_call(
        &app,
        "POST",
        "/mcp",
        None,
        Some(rpc(3, "tools/list", json!({}))),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{v}");

    // An on-chain tool answers unavailable_in_browser instead of failing.
    let (s, text) = call(
        &app,
        "POST",
        "/mcp",
        Some(&key),
        Some(rpc(
            3,
            "tools/call",
            json!({"name": "sync_zcash", "arguments": {}}),
        )),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{text}");
    assert!(text.contains("unavailable_in_browser"), "{text}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mgmt_refuses_bodies_with_unknown_fields() {
    // A JSON-RPC or chat-completion body aimed at a /_mgmt route (say through
    // a model-written MCP or provider URL) must not be read as a valid call
    // that happens to ignore the extras.
    let mock = start_mock();
    let dir = tempfile::tempdir().unwrap();
    let app = app(&mock, &dir);
    let (s, v) = json_call(
        &app,
        "POST",
        "/_mgmt/init",
        None,
        Some(json!({"password": "pw", "jsonrpc": "2.0", "method": "tools/list"})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    let (_, v) = json_call(&app, "GET", "/_mgmt/status", None, None).await;
    assert_eq!(v["initialized"], false, "{v}");

    let (s, v) = json_call(
        &app,
        "POST",
        "/_mgmt/generate",
        None,
        Some(json!({"model": "default", "messages": []})),
    )
    .await;
    assert!(s.is_client_error(), "{s} {v}");
}
