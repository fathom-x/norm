//! owallet-web in a real browser (main thread, in-memory storage), against
//! the mock Overpay. Run via the harness in README.md
//! (`wasm-bindgen-test-runner` + headless Chromium).
#![cfg(all(target_family = "wasm", target_os = "unknown"))]

mod common;

use common::*;
use serde_json::{json, Value};
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen_test]
async fn health_reports_the_owallet_version() {
    init("memory", "health.db").await;
    let (s, v) = json_call("GET", "/health", None, None).await;
    assert_eq!(s, 200);
    assert_eq!(
        v,
        json!({"name": "owallet", "version": owallet_mcp::VERSION})
    );
}

#[wasm_bindgen_test]
async fn mgmt_bootstrap_then_v1_status_with_the_minted_key() {
    init("memory", "bootstrap.db").await;
    let (_, v) = json_call("GET", "/_mgmt/status", None, None).await;
    assert_eq!(
        (v["initialized"].as_bool(), v["wallet"].clone()),
        (Some(false), Value::Null)
    );

    let key = bootstrap().await;
    let (_, v) = json_call("GET", "/_mgmt/status", None, None).await;
    assert_eq!(v["unlocked"], true);
    assert_eq!(v["env"], "dev");
    assert!(v["wallet"]["npub"].as_str().unwrap().starts_with("npub1"));

    let (s, v) = json_call("GET", "/v1/status", Some(&key), None).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["key_can_spend"], true);
    assert_eq!(v["overpay_connected"], true, "{v}");
    assert!(v["merchant_credits"].is_array(), "{v}");
    assert!(v["balance_error"]
        .as_str()
        .unwrap()
        .starts_with("unavailable_in_browser"));

    let (s, v) = json_call("GET", "/v1/status", Some("owk_nope"), None).await;
    assert_eq!(s, 401, "{v}");

    // Zero-click link + credits.
    let (s, v) = json_call("POST", "/_mgmt/overpay/register", None, None).await;
    assert_eq!(s, 200, "{v}");
    let (_, v) = json_call("GET", "/_mgmt/status", None, None).await;
    assert_eq!(v["overpay_linked"], true);
    let (s, v) = json_call("GET", "/_mgmt/credits", None, None).await;
    assert_eq!(s, 200, "{v}");
    assert!(v["data"][0]["balance_cents"].is_number(), "{v}");

    // Demo credits: the mock offers none unless MOCK_DEMO_CREDITS_CENTS is
    // set, and Overpay's refusal keeps its status and code through /_mgmt.
    let (s, v) = json_call("GET", "/_mgmt/demo-credits", None, None).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["data"]["enabled"], false, "{v}");
    let (s, v) = json_call("POST", "/_mgmt/demo-credits", None, None).await;
    assert_eq!(s, 404, "{v}");
    assert_eq!(v["error"]["code"], "demo_credits_disabled", "{v}");
}

#[wasm_bindgen_test]
async fn v1_models_lists_the_marketplace_models() {
    init("memory", "models.db").await;
    let key = bootstrap().await;
    let (s, v) = json_call("GET", "/v1/models", Some(&key), None).await;
    assert_eq!(s, 200, "{v}");
    let ids: Vec<&str> = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["default", "mock/chat-small", "mock/chat-large"]);
    let mini = &v["data"][1];
    assert!(mini["pricing"].is_object(), "{mini}");
    assert_eq!(mini["context_length"], 400000);
}

#[wasm_bindgen_test]
async fn streamed_chat_completion_arrives_in_chunks() {
    init("memory", "stream.db").await;
    let key = bootstrap().await;
    let resp = fetch(
        "POST",
        "/v1/chat/completions",
        Some(&key),
        Some(json!({
            "model": "mock/chat-small",
            "stream": true,
            "messages": [{"role": "user", "content": "hi"}],
        })),
    )
    .await;
    assert_eq!(resp.status(), 200);
    assert!(resp
        .headers()
        .get("content-type")
        .unwrap()
        .unwrap()
        .starts_with("text/event-stream"));
    let chunks = read_chunks(&resp).await;
    // Frames arrive as the order progresses, not as one buffered body.
    assert!(chunks.len() > 2, "{chunks:?}");
    let text = chunks.concat();
    assert!(text.trim_end().ends_with("data: [DONE]"), "{text}");
    let events: Vec<Value> = text
        .lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .filter(|d| *d != "[DONE]")
        .map(|d| serde_json::from_str(d).unwrap())
        .collect();
    let deltas: Vec<&str> = events
        .iter()
        .filter_map(|e| {
            e.pointer("/choices/0/delta/content")
                .and_then(Value::as_str)
        })
        .collect();
    assert!(!deltas.is_empty(), "{text}");
    assert_eq!(deltas.concat(), "Hello from the mock seller.");
    let usage = events
        .iter()
        .find_map(|e| e.get("usage").filter(|u| !u.is_null()))
        .expect("usage");
    assert_eq!(usage["charged_cents"], 3, "{usage}");
}

#[wasm_bindgen_test]
async fn buffered_chat_completion() {
    init("memory", "buffered.db").await;
    let key = bootstrap().await;
    let (s, v) = json_call(
        "POST",
        "/v1/chat/completions",
        Some(&key),
        Some(json!({"model": "default", "messages": [{"role": "user", "content": "hi"}]})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        v["choices"][0]["message"]["content"],
        "Hello from the mock seller."
    );
    assert!(v["usage"]["charged_cents"].is_number(), "{v}");
}

#[wasm_bindgen_test]
async fn mcp_tools_list_with_a_provider_key() {
    init("memory", "mcp.db").await;
    let key = bootstrap().await;
    let (s, v) = json_call(
        "POST",
        "/mcp",
        Some(&key),
        Some(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    let names: Vec<&str> = v["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"get_account_info"), "{names:?}");
    assert!(names.contains(&"run_python"), "{names:?}");
}

#[wasm_bindgen_test]
async fn mcp_tools_call_streams_over_sse() {
    init("memory", "mcp-sse.db").await;
    let key = bootstrap().await;
    let resp = fetch(
        "POST",
        "/mcp",
        Some(&key),
        Some(
            json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {
                "name": "run_python", "arguments": {"code": "print(6*7)"},
                "_meta": {"progressToken": "p1"},
            }}),
        ),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let text = read_chunks(&resp).await.concat();
    let last = text
        .lines()
        .rev()
        .find_map(|l| l.strip_prefix("data: "))
        .expect("an event");
    let v: Value = serde_json::from_str(last).unwrap();
    assert_eq!(v["id"], 7, "{text}");
    assert_eq!(v["result"]["isError"], false, "{text}");
    assert!(
        v["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("42"),
        "{text}"
    );
}

/// The Python wallet's DB fixture, loaded into the in-memory VFS, unlocks
/// and decrypts through the browser's SQLite exactly as natively.
#[wasm_bindgen_test]
async fn python_fixture_db_unlocks_in_the_browser() {
    let bytes = include_bytes!("../../owallet-db/tests/fixtures/python_v0_1_0.db");
    owallet_web::import_memory_db("python_v0_1_0.db", bytes).unwrap();
    init("memory", "python_v0_1_0.db").await;
    let (_, v) = json_call("GET", "/_mgmt/status", None, None).await;
    assert_eq!(
        (v["initialized"].as_bool(), v["unlocked"].as_bool()),
        (Some(true), Some(false)),
        "{v}"
    );
    let (s, v) = json_call(
        "POST",
        "/_mgmt/unlock",
        None,
        Some(json!({"password": "wrong"})),
    )
    .await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (401, Some("bad_password"))
    );
    let (s, v) = json_call(
        "POST",
        "/_mgmt/unlock",
        None,
        Some(json!({"password": "fixture-pw"})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["unlocked"], true);
    // Selecting the fixture's wallet decrypts its seed, and so does
    // minting a key for it.
    let (s, v) = json_call(
        "POST",
        "/_mgmt/select",
        None,
        Some(json!({"npub": "npub1fixturefixturefixture"})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    let (s, v) = json_call(
        "POST",
        "/_mgmt/provider-key/create",
        None,
        Some(json!({"label": "compat"})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["npub"], "npub1fixturefixturefixture");
}
