//! OPFS persistence: the sync-access-handle pool only exists in dedicated
//! workers, so this suite runs in one.
#![cfg(all(target_family = "wasm", target_os = "unknown"))]

mod common;

use common::*;
use serde_json::json;
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_dedicated_worker);

#[wasm_bindgen_test]
async fn opfs_database_survives_a_reinit() {
    let name = format!("persist-{}.db", js_sys::Date::now() as u64);
    init("opfs", &name).await;
    let key = bootstrap().await;
    let (_, before) = json_call("GET", "/_mgmt/status", None, None).await;

    // A fresh router over the same OPFS file: locked, same wallet.
    init("opfs", &name).await;
    let (_, v) = json_call("GET", "/_mgmt/status", None, None).await;
    assert_eq!(v["initialized"], true, "{v}");
    assert_eq!(v["unlocked"], false, "{v}");
    assert_eq!(v["wallet"], before["wallet"]);
    let (s, v) = json_call(
        "POST",
        "/_mgmt/unlock",
        None,
        Some(json!({"password": "pw"})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    let (s, v) = json_call("GET", "/v1/status", Some(&key), None).await;
    assert_eq!(s, 200, "{v}");
}
