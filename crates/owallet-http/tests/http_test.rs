//! End-to-end HTTP tests against the real axum router.
//!
//! Each test spins up an in-memory `Database` (via a tempdir) and uses
//! `axum_test::TestServer` to drive the router as if it were a live HTTP
//! server. Cookies persist across requests within one `TestServer`.

use std::sync::Arc;

use axum::http::StatusCode;
use axum_test::TestServer;
use owallet_crypto::{derive_from_mnemonic, npub_from_private_key, Address, Mnemonic, EVM_HD_PATH};
use owallet_db::Database;
use owallet_http::{build_router, AppState, EvmConfig};
use owallet_overpay::OverpayClient;
use serde_json::{json, Value};
use tempfile::TempDir;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const MASTER_PW: &str = "master-pw";

const ABANDON_12: &str = "abandon abandon abandon abandon abandon abandon \
     abandon abandon abandon abandon abandon about";

/// The well-known address for the canonical abandon-mnemonic at the
/// standard EVM derivation path (lowercase form).
const ABANDON_ADDRESS_LOWER: &str = "0x9858effd232b4033e47d90003d41ec34ecaeda94";

struct Harness {
    server: TestServer,
    _tmp: TempDir,
}

fn harness() -> Harness {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("test.db");
    let db = Database::init(&path, MASTER_PW).expect("init");
    let state = AppState::for_test(db);
    let app = build_router(state);
    let mut server = TestServer::new(app).expect("server");
    server.save_cookies();
    Harness { server, _tmp: tmp }
}

/// Re-open the underlying DB file and seed it with the canonical
/// abandon-mnemonic wallet plus a per-wallet password.
fn seed_abandon_wallet_via_file(tmp: &TempDir, wallet_pw: &str) -> String {
    let path = tmp.path().join("test.db");
    let mut db = Database::open(&path).unwrap();
    assert!(db.unlock(MASTER_PW).unwrap());
    let m = Mnemonic::parse(ABANDON_12).unwrap();
    let sk = derive_from_mnemonic(&m, EVM_HD_PATH).unwrap();
    let address = Address::from_private_key(&sk).to_hex_lower();
    let npub = npub_from_private_key(&sk).unwrap();
    db.write_wallet(&npub, ABANDON_12, Some(&address)).unwrap();
    db.write_default_npub(&npub).unwrap();
    db.write_wallet_password(&npub, wallet_pw).unwrap();
    npub
}

// ---------------------------------------------------------------------------
// Anonymous browsing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn root_redirects_to_wallet() {
    let h = harness();
    let res = h.server.get("/").await;
    res.assert_status(StatusCode::PERMANENT_REDIRECT);
    assert_eq!(res.header("location").to_str().unwrap(), "/wallet");
}

#[tokio::test]
async fn wallet_redirects_to_login_when_anonymous() {
    let h = harness();
    let res = h.server.get("/wallet").await;
    // Redirect::to() uses 303 See Other by default in axum 0.8.
    res.assert_status(StatusCode::SEE_OTHER);
    assert_eq!(res.header("location").to_str().unwrap(), "/wallet/login");
}

#[tokio::test]
async fn login_page_renders() {
    let h = harness();
    let res = h.server.get("/wallet/login").await;
    res.assert_status_ok();
    let body = res.text();
    assert!(body.contains("Sign in to owallet"));
    assert!(body.contains("name=\"identifier\""));
    assert!(body.contains("name=\"password\""));
}

// ---------------------------------------------------------------------------
// Admin login flow
// ---------------------------------------------------------------------------

#[tokio::test]
async fn admin_login_succeeds_and_sets_cookie() {
    let h = harness();

    let res = h
        .server
        .post("/wallet/login")
        .form(&json!({
            "role": "admin",
            "identifier": "",
            "password": MASTER_PW,
        }))
        .await;
    res.assert_status(StatusCode::SEE_OTHER);
    assert_eq!(res.header("location").to_str().unwrap(), "/wallet");

    // The dashboard now renders without redirect.
    let dash = h.server.get("/wallet").await;
    dash.assert_status_ok();
    let body = dash.text();
    assert!(body.contains("admin"));
    assert!(body.contains("Account"));
}

#[tokio::test]
async fn admin_login_wrong_password_returns_401() {
    let h = harness();
    let res = h
        .server
        .post("/wallet/login")
        .form(&json!({
            "role": "admin",
            "identifier": "",
            "password": "nope",
        }))
        .await;
    res.assert_status(StatusCode::UNAUTHORIZED);
    assert!(res.text().contains("Invalid credentials"));
}

#[tokio::test]
async fn logout_clears_session() {
    let h = harness();
    h.server
        .post("/wallet/login")
        .form(&json!({
            "role": "admin",
            "identifier": "",
            "password": MASTER_PW,
        }))
        .await
        .assert_status(StatusCode::SEE_OTHER);

    h.server.get("/wallet").await.assert_status_ok();

    h.server
        .post("/wallet/logout")
        .await
        .assert_status(StatusCode::SEE_OTHER);
    h.server
        .get("/wallet")
        .await
        .assert_status(StatusCode::SEE_OTHER);
}

// ---------------------------------------------------------------------------
// Wallet (per-wallet password) login
// ---------------------------------------------------------------------------

#[tokio::test]
async fn wallet_login_with_seeded_wallet() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("test.db");
    let db = Database::init(&path, MASTER_PW).unwrap();
    let state = AppState::for_test(db);
    let app = build_router(state);
    let mut server = TestServer::new(app).unwrap();
    server.save_cookies();

    let npub = seed_abandon_wallet_via_file(&tmp, "wallet-pw");

    server
        .post("/wallet/login")
        .form(&json!({
            "role": "wallet",
            "identifier": ABANDON_ADDRESS_LOWER,
            "password": "wallet-pw",
        }))
        .await
        .assert_status(StatusCode::SEE_OTHER);

    let body = server.get("/wallet").await.text();
    assert!(
        body.contains(&npub),
        "dashboard should list the wallet's npub"
    );
    // Wallet sessions do NOT see admin features.
    assert!(!body.contains("Generate new wallet"));
}

// ---------------------------------------------------------------------------
// Admin-only operations
// ---------------------------------------------------------------------------

async fn login_admin(server: &TestServer) {
    server
        .post("/wallet/login")
        .form(&json!({
            "role": "admin",
            "identifier": "",
            "password": MASTER_PW,
        }))
        .await
        .assert_status(StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn admin_generate_creates_wallet_and_shows_seed_once() {
    let h = harness();
    login_admin(&h.server).await;

    let res = h
        .server
        .post("/wallet/generate")
        .form(&json!({ "words": "12", "wallet_password": "wpw", "confirm_password": "wpw" }))
        .await;
    res.assert_status_ok();
    let body = res.text();
    assert!(body.contains("Wallet generated"));
    assert!(body.contains("Seed phrase"));
    assert!(body.contains("npub1"));
}

#[tokio::test]
async fn anonymous_cannot_generate() {
    let h = harness();
    h.server
        .post("/wallet/generate")
        .form(&json!({ "words": "12", "wallet_password": "wpw", "confirm_password": "wpw" }))
        .await
        .assert_status(StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn admin_can_import_known_mnemonic() {
    let h = harness();
    login_admin(&h.server).await;

    h.server
        .post("/wallet/import")
        .form(
            &json!({ "material": ABANDON_12, "wallet_password": "wpw", "confirm_password": "wpw" }),
        )
        .await
        .assert_status(StatusCode::SEE_OTHER);

    let body = h.server.get("/wallet").await.text();
    assert!(body.contains(ABANDON_ADDRESS_LOWER));
}

#[tokio::test]
async fn provider_key_budget_is_set_at_creation_and_editable_from_the_dashboard() {
    let h = harness();
    let npub = seed_abandon_wallet_via_file(&h._tmp, "wallet-pw");
    login_admin(&h.server).await;

    // Create a spend key with a $25 lifetime budget.
    let res = h
        .server
        .post("/wallet/provider-keys")
        .form(&json!({"allow_spend": "on", "budget_usd": "25"}))
        .await;
    res.assert_status_ok();
    assert!(
        res.text().contains("$25.00"),
        "created page shows the budget"
    );

    let db_path = h._tmp.path().join("test.db");
    let key_id = {
        let mut db = Database::open(&db_path).unwrap();
        assert!(db.unlock(MASTER_PW).unwrap());
        let listed = db.list_provider_keys(&npub).unwrap();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].can_spend());
        assert_eq!(listed[0].daily_budget_usd_cents, Some(2500));
        listed[0].id.clone()
    };

    // The dashboard row shows remaining-of-daily-total and the edit form.
    let dash = h.server.get("/wallet").await.text();
    assert!(
        dash.contains("$25.00 left today of $25.00/day"),
        "budget cell renders"
    );

    // Blank budget clears the limit.
    let res = h
        .server
        .post("/wallet/provider-keys/budget")
        .form(&json!({"id": key_id, "budget_usd": ""}))
        .await;
    res.assert_status(StatusCode::SEE_OTHER);
    {
        let mut db = Database::open(&db_path).unwrap();
        assert!(db.unlock(MASTER_PW).unwrap());
        let listed = db.list_provider_keys(&npub).unwrap();
        assert_eq!(listed[0].daily_budget_usd_cents, None);
    }
    let dash = h.server.get("/wallet").await.text();
    assert!(dash.contains("no limit"));

    // An invalid budget is refused with a notice and changes nothing.
    let res = h
        .server
        .post("/wallet/provider-keys/budget")
        .form(&json!({"id": key_id, "budget_usd": "-5"}))
        .await;
    res.assert_status(StatusCode::SEE_OTHER);
    let loc = res.header("location");
    assert!(loc
        .to_str()
        .unwrap()
        .contains("provider-key-budget-invalid"));
    {
        let mut db = Database::open(&db_path).unwrap();
        assert!(db.unlock(MASTER_PW).unwrap());
        assert_eq!(
            db.list_provider_keys(&npub).unwrap()[0].daily_budget_usd_cents,
            None
        );
    }
}

#[tokio::test]
async fn wallet_timezone_is_settable_validated_and_shown_on_the_dashboard() {
    let h = harness();
    login_admin(&h.server).await;

    // Default renders as UTC.
    let dash = h.server.get("/wallet").await.text();
    assert!(dash.contains("Time zone"));
    assert!(dash.contains("value=\"UTC\""));

    // A valid IANA name is stored and echoed back.
    let res = h
        .server
        .post("/wallet/settings/timezone")
        .form(&json!({"timezone": "Europe/Berlin"}))
        .await;
    res.assert_status(StatusCode::SEE_OTHER);
    let dash = h.server.get("/wallet").await.text();
    assert!(dash.contains("value=\"Europe/Berlin\""));
    {
        let mut db = Database::open(&h._tmp.path().join("test.db")).unwrap();
        assert!(db.unlock(MASTER_PW).unwrap());
        assert_eq!(
            db.read_timezone().unwrap().as_deref(),
            Some("Europe/Berlin")
        );
    }

    // Garbage is refused with a notice and the setting stays put.
    let res = h
        .server
        .post("/wallet/settings/timezone")
        .form(&json!({"timezone": "Mars/Olympus_Mons"}))
        .await;
    res.assert_status(StatusCode::SEE_OTHER);
    assert!(res
        .header("location")
        .to_str()
        .unwrap()
        .contains("timezone-invalid"));
    {
        let mut db = Database::open(&h._tmp.path().join("test.db")).unwrap();
        assert!(db.unlock(MASTER_PW).unwrap());
        assert_eq!(
            db.read_timezone().unwrap().as_deref(),
            Some("Europe/Berlin")
        );
    }

    // Blank resets to UTC.
    let res = h
        .server
        .post("/wallet/settings/timezone")
        .form(&json!({"timezone": ""}))
        .await;
    res.assert_status(StatusCode::SEE_OTHER);
    let mut db = Database::open(&h._tmp.path().join("test.db")).unwrap();
    assert!(db.unlock(MASTER_PW).unwrap());
    assert_eq!(db.read_timezone().unwrap().as_deref(), Some("UTC"));
}

#[tokio::test]
async fn spend_cap_is_settable_cleared_and_prefilled_on_the_dashboard() {
    let h = harness();
    seed_abandon_wallet_via_file(&h._tmp, "wallet-pw");
    login_admin(&h.server).await;
    let db_path = h._tmp.path().join("test.db");

    // Set a $5 wallet-level cap.
    let res = h
        .server
        .post("/wallet/settings/spend-cap")
        .form(&json!({"spend_cap_usd": "5"}))
        .await;
    res.assert_status(StatusCode::SEE_OTHER);
    {
        let mut db = Database::open(&db_path).unwrap();
        assert!(db.unlock(MASTER_PW).unwrap());
        assert_eq!(db.read_spend_cap_usd_cents().unwrap(), Some(500));
    }
    let dash = h.server.get("/wallet").await.text();
    assert!(dash.contains("value=\"5.00\""), "prefilled cap: {dash}");

    // Garbage is refused with a notice; the setting stays put.
    let res = h
        .server
        .post("/wallet/settings/spend-cap")
        .form(&json!({"spend_cap_usd": "-3"}))
        .await;
    assert!(res
        .header("location")
        .to_str()
        .unwrap()
        .contains("spend-cap-invalid"));
    {
        let mut db = Database::open(&db_path).unwrap();
        assert!(db.unlock(MASTER_PW).unwrap());
        assert_eq!(db.read_spend_cap_usd_cents().unwrap(), Some(500));
    }

    // Blank clears the override (back to the server default).
    h.server
        .post("/wallet/settings/spend-cap")
        .form(&json!({"spend_cap_usd": ""}))
        .await
        .assert_status(StatusCode::SEE_OTHER);
    let mut db = Database::open(&db_path).unwrap();
    assert!(db.unlock(MASTER_PW).unwrap());
    assert_eq!(db.read_spend_cap_usd_cents().unwrap(), None);
}

#[tokio::test]
async fn budget_on_a_chat_only_key_is_stored_and_bounds_operating_spend() {
    let h = harness();
    let npub = seed_abandon_wallet_via_file(&h._tmp, "wallet-pw");
    login_admin(&h.server).await;

    // No allow_spend tick: the key is chat-only, but chat turns are paid
    // orders, so the daily budget applies to it all the same.
    h.server
        .post("/wallet/provider-keys")
        .form(&json!({"budget_usd": "10"}))
        .await
        .assert_status_ok();

    let mut db = Database::open(&h._tmp.path().join("test.db")).unwrap();
    assert!(db.unlock(MASTER_PW).unwrap());
    let listed = db.list_provider_keys(&npub).unwrap();
    assert_eq!(listed.len(), 1);
    assert!(!listed[0].can_spend());
    assert_eq!(listed[0].daily_budget_usd_cents, Some(1000));
}

#[tokio::test]
async fn import_bad_material_shows_error_on_same_page() {
    let h = harness();
    login_admin(&h.server).await;

    let res = h
        .server
        .post("/wallet/import")
        .form(&json!({ "material": "definitely not a mnemonic" }))
        .await;
    res.assert_status_ok();
    let body = res.text();
    assert!(body.contains("Import an existing wallet"));
    assert!(body.contains("HD derivation") || body.contains("hex") || body.contains("mnemonic"));
}

// ---------------------------------------------------------------------------
// POST /wallet/send (real on-chain transfer via wiremock'd JSON-RPC)
// ---------------------------------------------------------------------------

/// Same shape as the JSON-RPC mock in `owallet-evm/tests/send_test.rs` and
/// `owallet-http/tests/mcp_test.rs`. Kept as a third copy for the same
/// reason — see the matching comment in those files. Adds an
/// `eth_call` branch that returns a fixed USDC `balanceOf` reading
/// (5_000_000 = 5 USDC at 6 decimals), so dashboard / MCP tests can
/// assert the on-chain-balance rendering path.
struct JsonRpcMock;

impl Respond for JsonRpcMock {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
        let method = body["method"].as_str().unwrap_or("");
        let id = body["id"].clone();
        let result: Value = match method {
            "eth_chainId" => json!("0x2105"),
            "eth_getTransactionCount" => json!("0x0"),
            "eth_getBalance" => json!("0xde0b6b3a7640000"),
            "eth_gasPrice" => json!("0x3b9aca00"),
            "eth_maxPriorityFeePerGas" => json!("0x3b9aca00"),
            "eth_feeHistory" => json!({
                "baseFeePerGas": ["0x1", "0x1"],
                "gasUsedRatio": [0.5_f64],
                "oldestBlock": "0x0",
                "reward": [["0x3b9aca00"]],
            }),
            "eth_estimateGas" => json!("0xea60"),
            "eth_blockNumber" => json!("0x1"),
            "eth_getBlockByNumber" => json!({"number": "0x1", "baseFeePerGas": "0x1"}),
            // ERC-20 balanceOf(address) returns uint256 → 5_000_000 = 5 USDC.
            "eth_call" => {
                json!("0x00000000000000000000000000000000000000000000000000000000004c4b40")
            }
            "eth_sendRawTransaction" => {
                json!("0x6d6b56b3acba7ebe1e44b8a5b1bb9d8a89e9ae37e9b97c4a4e0e0e0e0e0e0e0e")
            }
            "eth_getTransactionReceipt" => json!({
                "transactionHash":
                    "0x6d6b56b3acba7ebe1e44b8a5b1bb9d8a89e9ae37e9b97c4a4e0e0e0e0e0e0e0e",
                "transactionIndex": "0x0",
                "blockHash":
                    "0x0000000000000000000000000000000000000000000000000000000000000001",
                "blockNumber": "0x1234",
                "from": "0x9858effd232b4033e47d90003d41ec34ecaeda94",
                "to": "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913",
                "cumulativeGasUsed": "0xea60",
                "gasUsed": "0xea60",
                "contractAddress": null,
                "logs": [],
                "logsBloom": format!("0x{}", "0".repeat(512)),
                "type": "0x2",
                "effectiveGasPrice": "0x3b9aca00",
                "status": "0x1",
            }),
            _ => json!(null),
        };
        ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        }))
    }
}

/// Build a TestServer with the abandon wallet preloaded, an Overpay
/// pointed at an unused placeholder URL (the send flow never calls it),
/// and `rpc_url` set to the caller-supplied JSON-RPC mock URI.
async fn dashboard_with_rpc(rpc_url: String) -> (TestServer, TempDir) {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("test.db");
    let db = Database::init(&path, MASTER_PW).expect("init");
    let overpay = Arc::new(OverpayClient::new("http://127.0.0.1:1").unwrap());
    let evm = EvmConfig {
        rpc_url,
        network: "eip155:8453".into(),
    };
    let state = AppState::new(db, overpay, evm, "test-host".to_string());
    let app = build_router(state);
    let mut server = TestServer::new(app).unwrap();
    server.save_cookies();

    // Seed the abandon wallet so the send handler has a real key to derive.
    let mut db = Database::open(&path).unwrap();
    assert!(db.unlock(MASTER_PW).unwrap());
    let m = Mnemonic::parse(ABANDON_12).unwrap();
    let sk = derive_from_mnemonic(&m, EVM_HD_PATH).unwrap();
    let address = Address::from_private_key(&sk).to_hex_lower();
    let npub = npub_from_private_key(&sk).unwrap();
    db.write_wallet(&npub, ABANDON_12, Some(&address)).unwrap();
    db.write_default_npub(&npub).unwrap();
    drop(db);

    (server, tmp)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_post_signs_broadcasts_and_renders_result() {
    let rpc = MockServer::start().await;
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(JsonRpcMock)
        .mount(&rpc)
        .await;

    let (server, _tmp) = dashboard_with_rpc(rpc.uri()).await;
    login_admin(&server).await;

    let res = server
        .post("/wallet/send")
        .form(&json!({
            "to": "0x000000000000000000000000000000000000dead",
            "amount": "1.25",
        }))
        .await;
    res.assert_status_ok();
    let body = res.text();
    assert!(body.contains("Send confirmed"), "body: {body}");
    assert!(body.contains("0x6d6b56b3acba7ebe1e44b8a5b1bb9d8a89e9ae37e9b97c4a4e0e0e0e0e0e0e0e"));
    assert!(body.contains("1.25 USDC"));
    assert!(body.contains("Base"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_post_rejects_bad_recipient_address() {
    // No RPC needed — owallet_evm rejects the address before any call.
    let (server, _tmp) = dashboard_with_rpc("http://127.0.0.1:1".to_string()).await;
    login_admin(&server).await;

    let res = server
        .post("/wallet/send")
        .form(&json!({ "to": "not-an-address", "amount": "1.0" }))
        .await;
    res.assert_status(StatusCode::BAD_REQUEST);
    let body = res.text();
    assert!(body.contains("Send failed"), "body: {body}");
    assert!(body.contains("invalid recipient address") || body.contains("not-an-address"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_post_rejects_non_positive_amount() {
    let (server, _tmp) = dashboard_with_rpc("http://127.0.0.1:1".to_string()).await;
    login_admin(&server).await;

    let res = server
        .post("/wallet/send")
        .form(&json!({
            "to": "0x000000000000000000000000000000000000dead",
            "amount": "0",
        }))
        .await;
    res.assert_status(StatusCode::BAD_REQUEST);
    let body = res.text();
    assert!(
        body.contains("Amount must be a positive number"),
        "body: {body}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_post_redirects_anonymous_to_login() {
    let (server, _tmp) = dashboard_with_rpc("http://127.0.0.1:1".to_string()).await;
    let res = server
        .post("/wallet/send")
        .form(&json!({ "to": "0xabc", "amount": "1.0" }))
        .await;
    res.assert_status(StatusCode::SEE_OTHER);
    assert_eq!(res.header("location").to_str().unwrap(), "/wallet/login");
}

// ---------------------------------------------------------------------------
// Dashboard browser-OAuth (Link Overpay / Open Overpay)
// ---------------------------------------------------------------------------

/// Build a dashboard pointed at a wiremock'd Overpay base URL. The
/// abandon wallet is seeded as default so the OAuth handlers have an
/// active npub to bind the token to.
///
/// `issuer` is the dashboard's own externally-reachable base URL — it shapes
/// the OAuth redirect URI, *not* the key bearers are filed under. That key is
/// derived from `overpay_uri`.
async fn dashboard_with_overpay(overpay_uri: &str, issuer: &str) -> (TestServer, TempDir) {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("test.db");
    let db = Database::init(&path, MASTER_PW).expect("init");
    let overpay = Arc::new(OverpayClient::new(overpay_uri).unwrap());
    let evm = EvmConfig {
        rpc_url: "http://127.0.0.1:1".to_string(),
        network: "eip155:8453".into(),
    };
    let state = AppState::new(db, overpay, evm, issuer.to_string());
    let app = build_router(state);
    let mut server = TestServer::new(app).unwrap();
    server.save_cookies();

    let mut db = Database::open(&path).unwrap();
    assert!(db.unlock(MASTER_PW).unwrap());
    let m = Mnemonic::parse(ABANDON_12).unwrap();
    let sk = derive_from_mnemonic(&m, EVM_HD_PATH).unwrap();
    let address = Address::from_private_key(&sk).to_hex_lower();
    let npub = npub_from_private_key(&sk).unwrap();
    db.write_wallet(&npub, ABANDON_12, Some(&address)).unwrap();
    db.write_default_npub(&npub).unwrap();
    drop(db);

    (server, tmp)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wallet_authorize_registers_client_and_redirects_with_cookie() {
    use wiremock::matchers::{method as wm_method, path as wm_path};
    let overpay = MockServer::start().await;
    Mock::given(wm_method("POST"))
        .and(wm_path("/oauth/clients"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "client_id": "client_dashboard_test",
        })))
        .mount(&overpay)
        .await;

    let issuer = "http://owallet.test";
    let (server, _tmp) = dashboard_with_overpay(&overpay.uri(), issuer).await;
    login_admin(&server).await;

    let res = server.get("/wallet/authorize").await;
    res.assert_status(StatusCode::SEE_OTHER);
    let loc = res.header("location").to_str().unwrap().to_string();
    assert!(
        loc.contains("/oauth/authorize"),
        "should 302 to Overpay /oauth/authorize, got {loc}"
    );
    let parsed = url::Url::parse(&loc).unwrap();
    let pairs: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
    assert_eq!(
        pairs.get("client_id").map(String::as_str),
        Some("client_dashboard_test")
    );
    assert!(pairs.contains_key("code_challenge"));
    assert_eq!(
        pairs.get("code_challenge_method").map(String::as_str),
        Some("S256")
    );
    // The callback comes back to *this* dashboard, so the redirect URI is
    // built from the issuer, never from the Overpay host.
    assert_eq!(
        pairs.get("redirect_uri").map(String::as_str),
        Some(format!("{issuer}/wallet/authorize/callback").as_str())
    );

    // The pending-auth cookie should be set on the response.
    let mut found_cookie = false;
    for v in res.headers().get_all("set-cookie").iter() {
        if v.to_str()
            .unwrap_or("")
            .starts_with("owallet_pending_auth=")
        {
            found_cookie = true;
        }
    }
    assert!(found_cookie, "owallet_pending_auth cookie should be set");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wallet_authorize_callback_exchanges_code_and_stores_token() {
    use wiremock::matchers::{method as wm_method, path as wm_path};
    let overpay = MockServer::start().await;
    Mock::given(wm_method("POST"))
        .and(wm_path("/oauth/clients"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "client_id": "client_cb_test",
        })))
        .mount(&overpay)
        .await;
    Mock::given(wm_method("POST"))
        .and(wm_path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "the_dashboard_bearer",
            "token_type": "Bearer",
        })))
        .mount(&overpay)
        .await;
    Mock::given(wm_method("GET"))
        .and(wm_path("/api/v1/account"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "username": "alice-dashboard",
        })))
        .mount(&overpay)
        .await;

    let issuer = "http://owallet.test";
    let (server, tmp) = dashboard_with_overpay(&overpay.uri(), issuer).await;
    login_admin(&server).await;

    // 1) GET /wallet/authorize → captures state from the redirect Location.
    let init = server.get("/wallet/authorize").await;
    init.assert_status(StatusCode::SEE_OTHER);
    let loc = init.header("location").to_str().unwrap().to_string();
    let parsed = url::Url::parse(&loc).unwrap();
    let pairs: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
    let state = pairs.get("state").cloned().expect("state in URL");

    // 2) GET /wallet/authorize/callback with the captured state. The
    //    TestServer carries the cookie set in step 1 automatically.
    let cb = server
        .get("/wallet/authorize/callback")
        .add_query_param("code", "THE_CODE")
        .add_query_param("state", state.as_str())
        .await;
    cb.assert_status(StatusCode::SEE_OTHER);
    assert_eq!(
        cb.header("location").to_str().unwrap(),
        "/wallet?notice=authorized"
    );

    // 3) The bearer is filed under the *Overpay* host, which is the key
    //    `owallet authorize` and the MCP tools both look under — not under
    //    this dashboard's issuer URL.
    let db_path = tmp.path().join("test.db");
    let mut db = Database::open(&db_path).unwrap();
    assert!(db.unlock(MASTER_PW).unwrap());
    let npub = db.read_default_npub().unwrap().unwrap();
    let canonical = owallet_overpay::host_key(&overpay.uri());
    assert_eq!(
        db.read_token(&npub, &canonical).unwrap().as_deref(),
        Some("the_dashboard_bearer")
    );
    assert_eq!(db.read_token(&npub, issuer).unwrap(), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wallet_authorize_callback_rejects_state_mismatch() {
    use wiremock::matchers::{method as wm_method, path as wm_path};
    let overpay = MockServer::start().await;
    Mock::given(wm_method("POST"))
        .and(wm_path("/oauth/clients"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "client_id": "client_mismatch",
        })))
        .mount(&overpay)
        .await;

    let (server, _tmp) = dashboard_with_overpay(&overpay.uri(), "http://owallet.test").await;
    login_admin(&server).await;

    // Start a flow so the pending cookie is set.
    server
        .get("/wallet/authorize")
        .await
        .assert_status(StatusCode::SEE_OTHER);

    // Now POST a bogus state to the callback.
    let cb = server
        .get("/wallet/authorize/callback")
        .add_query_param("code", "ANY")
        .add_query_param("state", "not-the-real-state")
        .await;
    cb.assert_status(StatusCode::SEE_OTHER);
    let loc_hdr = cb.header("location");
    let loc = loc_hdr.to_str().unwrap();
    assert!(loc.contains("authorize-error"), "loc: {loc}");
    assert!(loc.contains("state-mismatch"), "loc: {loc}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wallet_overpay_login_redirects_to_session_url_when_token_stored() {
    use wiremock::matchers::{header as wm_header, method as wm_method, path as wm_path};
    let overpay = MockServer::start().await;
    Mock::given(wm_method("POST"))
        .and(wm_path("/api/v1/buyer/web_session"))
        .and(wm_header("authorization", "Bearer the_stored_bearer"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "url": "https://overpay.test/auto-login/abc123",
        })))
        .mount(&overpay)
        .await;

    let (server, tmp) = dashboard_with_overpay(&overpay.uri(), "http://owallet.test").await;
    login_admin(&server).await;

    // Pre-seed a bearer the way `owallet authorize` files it: under the
    // Overpay host.
    let db_path = tmp.path().join("test.db");
    let mut db = Database::open(&db_path).unwrap();
    assert!(db.unlock(MASTER_PW).unwrap());
    let npub = db.read_default_npub().unwrap().unwrap();
    db.write_token(
        &npub,
        &owallet_overpay::host_key(&overpay.uri()),
        "the_stored_bearer",
        "overpay-oauth",
    )
    .unwrap();
    drop(db);

    let res = server.get("/wallet/overpay-login").await;
    res.assert_status(StatusCode::SEE_OTHER);
    assert_eq!(
        res.header("location").to_str().unwrap(),
        "https://overpay.test/auto-login/abc123"
    );
}

/// The dashboard has to find bearers older builds filed under the issuer URL
/// too, and re-file them so the next read is a direct hit.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wallet_overpay_login_migrates_token_from_legacy_issuer_key() {
    use wiremock::matchers::{header as wm_header, method as wm_method, path as wm_path};
    let overpay = MockServer::start().await;
    Mock::given(wm_method("POST"))
        .and(wm_path("/api/v1/buyer/web_session"))
        .and(wm_header("authorization", "Bearer legacy_bearer"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "url": "https://overpay.test/auto-login/legacy",
        })))
        .mount(&overpay)
        .await;

    let issuer = "http://owallet.test";
    let (server, tmp) = dashboard_with_overpay(&overpay.uri(), issuer).await;
    login_admin(&server).await;

    let db_path = tmp.path().join("test.db");
    let mut db = Database::open(&db_path).unwrap();
    assert!(db.unlock(MASTER_PW).unwrap());
    let npub = db.read_default_npub().unwrap().unwrap();
    db.write_token(&npub, issuer, "legacy_bearer", "overpay-oauth")
        .unwrap();
    drop(db);

    let res = server.get("/wallet/overpay-login").await;
    res.assert_status(StatusCode::SEE_OTHER);
    assert_eq!(
        res.header("location").to_str().unwrap(),
        "https://overpay.test/auto-login/legacy"
    );

    let mut db = Database::open(&db_path).unwrap();
    assert!(db.unlock(MASTER_PW).unwrap());
    let canonical = owallet_overpay::host_key(&overpay.uri());
    assert_eq!(
        db.read_token(&npub, &canonical).unwrap().as_deref(),
        Some("legacy_bearer")
    );
    assert_eq!(db.read_token(&npub, issuer).unwrap(), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wallet_overpay_login_redirects_to_dashboard_when_no_token() {
    let overpay = MockServer::start().await;
    let (server, _tmp) = dashboard_with_overpay(&overpay.uri(), "http://owallet.test").await;
    login_admin(&server).await;

    let res = server.get("/wallet/overpay-login").await;
    res.assert_status(StatusCode::SEE_OTHER);
    assert_eq!(
        res.header("location").to_str().unwrap(),
        "/wallet?notice=run-authorize"
    );
}

// ---------------------------------------------------------------------------
// Dashboard on-chain balance rows
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dashboard_renders_eth_and_usdc_balances() {
    let rpc = MockServer::start().await;
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(JsonRpcMock)
        .mount(&rpc)
        .await;

    // dashboard_with_rpc seeds the abandon wallet as default; the mock
    // returns 1 ETH (eth_getBalance) and 5 USDC (eth_call → balanceOf).
    let (server, _tmp) = dashboard_with_rpc(rpc.uri()).await;
    login_admin(&server).await;

    let body = server.get("/wallet").await.text();
    assert!(body.contains("1 ETH"), "no ETH balance row in: {body}");
    assert!(body.contains("5 USDC"), "no USDC balance row in: {body}");
    assert!(body.contains("Base"), "no chain label in: {body}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dashboard_renders_balance_row_with_error_when_rpc_unreachable() {
    // Point at a black hole so both balance calls fail. The page should
    // still render (best-effort), with a "could not fetch" notice in
    // each balance row.
    let (server, _tmp) = dashboard_with_rpc("http://127.0.0.1:1".to_string()).await;
    login_admin(&server).await;

    let body = server.get("/wallet").await.text();
    assert!(
        body.contains("could not fetch"),
        "expected error notice in: {body}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wallet_authorize_requires_login() {
    let (server, _tmp) = dashboard_with_overpay("http://127.0.0.1:1", "http://owallet.test").await;
    let res = server.get("/wallet/authorize").await;
    res.assert_status(StatusCode::SEE_OTHER);
    assert_eq!(res.header("location").to_str().unwrap(), "/wallet/login");
}

// ---------------------------------------------------------------------------
// Select / delete (admin operations)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn admin_can_select_alternate_default_wallet() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("test.db");
    let db = Database::init(&path, MASTER_PW).unwrap();
    let state = AppState::for_test(db);
    let app = build_router(state);
    let mut server = TestServer::new(app).unwrap();
    server.save_cookies();
    login_admin(&server).await;

    // Generate a fresh wallet via the HTTP flow.
    server
        .post("/wallet/generate")
        .form(&json!({ "words": "12", "wallet_password": "wpw", "confirm_password": "wpw" }))
        .await
        .assert_status_ok();
    // And import the abandon wallet alongside it.
    server
        .post("/wallet/import")
        .form(
            &json!({ "material": ABANDON_12, "wallet_password": "wpw", "confirm_password": "wpw" }),
        )
        .await
        .assert_status(StatusCode::SEE_OTHER);

    // Pick the abandon wallet as default.
    let abandon_npub = {
        let m = Mnemonic::parse(ABANDON_12).unwrap();
        let sk = derive_from_mnemonic(&m, EVM_HD_PATH).unwrap();
        npub_from_private_key(&sk).unwrap()
    };
    server
        .post("/wallet/select")
        .form(&json!({ "npub": abandon_npub }))
        .await
        .assert_status(StatusCode::SEE_OTHER);

    let body = server.get("/wallet").await.text();
    assert!(body.contains(ABANDON_ADDRESS_LOWER));
}

#[tokio::test]
async fn admin_can_delete_wallet() {
    let h = harness();
    login_admin(&h.server).await;

    h.server
        .post("/wallet/import")
        .form(
            &json!({ "material": ABANDON_12, "wallet_password": "wpw", "confirm_password": "wpw" }),
        )
        .await
        .assert_status(StatusCode::SEE_OTHER);

    let abandon_npub = {
        let m = Mnemonic::parse(ABANDON_12).unwrap();
        let sk = derive_from_mnemonic(&m, EVM_HD_PATH).unwrap();
        npub_from_private_key(&sk).unwrap()
    };

    h.server
        .post("/wallet/delete")
        .form(&json!({ "npub": abandon_npub }))
        .await
        .assert_status(StatusCode::SEE_OTHER);

    let body = h.server.get("/wallet").await.text();
    assert!(!body.contains(ABANDON_ADDRESS_LOWER));
}

// ---------------------------------------------------------------------------
// Password (per-wallet) management
// ---------------------------------------------------------------------------

#[tokio::test]
async fn admin_can_set_per_wallet_password_and_then_log_in_as_that_wallet() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("test.db");
    let db = Database::init(&path, MASTER_PW).unwrap();
    let state = AppState::for_test(db);
    let app = build_router(state);
    let mut server = TestServer::new(app).unwrap();
    server.save_cookies();
    login_admin(&server).await;

    server
        .post("/wallet/import")
        .form(
            &json!({ "material": ABANDON_12, "wallet_password": "wpw", "confirm_password": "wpw" }),
        )
        .await
        .assert_status(StatusCode::SEE_OTHER);
    let abandon_npub = {
        let m = Mnemonic::parse(ABANDON_12).unwrap();
        let sk = derive_from_mnemonic(&m, EVM_HD_PATH).unwrap();
        npub_from_private_key(&sk).unwrap()
    };

    server
        .post("/wallet/password")
        .form(&json!({
            "npub": abandon_npub,
            "password": "wallet-pw",
            "confirm": "wallet-pw",
        }))
        .await
        .assert_status(StatusCode::SEE_OTHER);

    // Logout admin, log in as the wallet.
    server
        .post("/wallet/logout")
        .await
        .assert_status(StatusCode::SEE_OTHER);
    server
        .post("/wallet/login")
        .form(&json!({
            "role": "wallet",
            "identifier": ABANDON_ADDRESS_LOWER,
            "password": "wallet-pw",
        }))
        .await
        .assert_status(StatusCode::SEE_OTHER);

    let body = server.get("/wallet").await.text();
    assert!(body.contains(&abandon_npub));
}

// ---------------------------------------------------------------------------
// /wallet/purchases — local purchase cache UI
// ---------------------------------------------------------------------------

fn abandon_npub_str() -> String {
    let m = Mnemonic::parse(ABANDON_12).unwrap();
    let sk = derive_from_mnemonic(&m, EVM_HD_PATH).unwrap();
    npub_from_private_key(&sk).unwrap()
}

fn seed_purchase(tmp: &TempDir, npub: &str, order: Value) {
    let path = tmp.path().join("test.db");
    let mut db = Database::open(&path).unwrap();
    assert!(db.unlock(MASTER_PW).unwrap());
    db.upsert_purchase(npub, &order).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn purchases_list_renders_cached_rows() {
    let (server, tmp) = dashboard_with_overpay("http://127.0.0.1:1", "http://owallet.test").await;
    login_admin(&server).await;
    seed_purchase(
        &tmp,
        &abandon_npub_str(),
        json!({
            "order_id": "o1", "product_title": "Widget",
            "seller_slug": "acme", "fulfillment_status": "delivered",
            "total_usd_cents": 500, "delivered_at": 1700000000_i64,
        }),
    );

    let body = server.get("/wallet/purchases").await.text();
    assert!(body.contains("Widget"), "body: {body}");
    assert!(body.contains("acme"));
    assert!(body.contains("delivered"));
    assert!(body.contains("$5.00"));
    assert!(body.contains("/wallet/purchases/o1"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn purchases_list_empty_state() {
    let (server, _tmp) = dashboard_with_overpay("http://127.0.0.1:1", "http://owallet.test").await;
    login_admin(&server).await;
    let body = server.get("/wallet/purchases").await.text();
    assert!(body.contains("No purchases cached yet"), "body: {body}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn purchase_detail_renders_delivered_content() {
    let (server, tmp) = dashboard_with_overpay("http://127.0.0.1:1", "http://owallet.test").await;
    login_admin(&server).await;
    seed_purchase(
        &tmp,
        &abandon_npub_str(),
        json!({
            "order_id": "o1", "product_title": "Secret Note",
            "fulfillment_status": "delivered",
            "delivered_content": "the answer is 42",
            "delivered_content_type": "text/plain",
        }),
    );

    let body = server.get("/wallet/purchases/o1").await.text();
    assert!(body.contains("Secret Note"), "body: {body}");
    assert!(body.contains("the answer is 42"));
    assert!(body.contains("Delivered content"));
}

/// The buyer's own submitted input (code to run, a prompt, …) renders as an
/// "Order input" section — schema-driven listings store it as a JSON object
/// serialized to a string, one labeled block per field.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn purchase_detail_renders_the_buyer_note() {
    let (server, tmp) = dashboard_with_overpay("http://127.0.0.1:1", "http://owallet.test").await;
    login_admin(&server).await;
    seed_purchase(
        &tmp,
        &abandon_npub_str(),
        json!({
            "order_id": "o2", "product_title": "Run Python Code",
            "fulfillment_status": "delivered",
            "buyer_note": "{\"code\": \"print(1 +\\n2)\", \"stdin\": \"\"}",
            "delivered_content": "3",
            "delivered_content_type": "text/plain",
        }),
    );

    let body = server.get("/wallet/purchases/o2").await.text();
    assert!(body.contains("Order input"), "body: {body}");
    assert!(body.contains("code"));
    assert!(body.contains("print(1 +"));
    // The deliverable leads the page; the input the buyer already knows
    // trails it.
    let content_pos = body.find("Delivered content").unwrap();
    let input_pos = body.find("Order input").unwrap();
    assert!(content_pos < input_pos, "output should render above input");

    // A plain-text note renders too; an absent one hides the section.
    seed_purchase(
        &tmp,
        &abandon_npub_str(),
        json!({"order_id": "o3", "buyer_note": "leave at the door"}),
    );
    let body = server.get("/wallet/purchases/o3").await.text();
    assert!(body.contains("Order input"));
    assert!(body.contains("leave at the door"));

    seed_purchase(&tmp, &abandon_npub_str(), json!({"order_id": "o4"}));
    let body = server.get("/wallet/purchases/o4").await.text();
    assert!(!body.contains("Order input"), "body: {body}");

    // A /v1-provider order: `messages` renders as a role-labeled transcript
    // (including assistant tool calls) and `tools` as a compact definition
    // list with the JSON-schema parameters folded away — not raw JSON blobs.
    let note = json!({
        "messages": [
            {"role": "user", "content": "add the numbers"},
            {"role": "assistant", "tool_calls": [
                {"type": "function", "function": {"name": "run_python", "arguments": "{\"code\": \"print(1+2)\"}"}}
            ]},
            {"role": "tool", "content": "3"}
        ],
        "model": "openai/gpt-5-mini",
        "tools": [
            {"type": "function", "function": {
                "name": "run_python",
                "description": "Run Python code",
                "parameters": {"type": "object", "properties": {"code": {"type": "string"}}}
            }}
        ]
    });
    seed_purchase(
        &tmp,
        &abandon_npub_str(),
        json!({"order_id": "o5", "buyer_note": note.to_string()}),
    );
    let body = server.get("/wallet/purchases/o5").await.text();
    assert!(body.contains("add the numbers"), "body: {body}");
    assert!(body.contains("assistant"));
    assert!(body.contains("→ run_python("));
    assert!(body.contains("<details"));
    assert!(body.contains("Run Python code"));
    // The transcript replaced the raw-JSON fallback for messages.
    assert!(!body.contains("&quot;role&quot;"), "body: {body}");
}

/// `x-widget: markdown` delivered content renders as real HTML — formatting
/// comes through, seller-authored markup does not: raw HTML is escaped and
/// non-http(s) link destinations are neutralized.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn purchase_detail_renders_markdown_as_html() {
    let (server, tmp) = dashboard_with_overpay("http://127.0.0.1:1", "http://owallet.test").await;
    login_admin(&server).await;
    let content = json!({
        "credits_refunded": false,
        "description": "## Result\n\nThe answer is **42**.\n\n<script>alert(1)</script>\n\n[click](javascript:alert(2))"
    });
    seed_purchase(
        &tmp,
        &abandon_npub_str(),
        json!({
            "order_id": "omd", "product_title": "Inference",
            "fulfillment_status": "delivered",
            "delivered_content": content.to_string(),
            "delivered_content_type": "application/json",
            "listing": {"delivered_content_schema": {
                "properties": {
                    "credits_refunded": {"type": "boolean"},
                    "description": {"x-widget": "markdown"}
                }
            }},
        }),
    );

    let body = server.get("/wallet/purchases/omd").await.text();
    assert!(body.contains("<h2>Result</h2>"), "body: {body}");
    assert!(body.contains("<strong>42</strong>"));
    // The content-bearing markdown field renders above scalar metadata,
    // even though "credits_refunded" sorts alphabetically before
    // "description".
    let answer = body.find("<h2>Result</h2>").unwrap();
    let meta = body.find("Credits Refunded").unwrap();
    assert!(answer < meta, "the answer should lead the metadata");
    assert!(
        !body.contains("<script>alert(1)</script>"),
        "raw HTML must be escaped"
    );
    assert!(body.contains("&lt;script&gt;"));
    assert!(
        !body.contains("javascript:alert"),
        "unsafe link scheme must be dropped"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn purchase_detail_404_for_uncached() {
    let (server, _tmp) = dashboard_with_overpay("http://127.0.0.1:1", "http://owallet.test").await;
    login_admin(&server).await;
    let body = server.get("/wallet/purchases/nope").await.text();
    assert!(body.contains("not in local cache"), "body: {body}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn purchases_sync_backfills_and_redirects_with_notice() {
    use wiremock::matchers::{method as wm_method, path as wm_path, path_regex as wm_path_regex};
    let overpay = MockServer::start().await;
    Mock::given(wm_method("GET"))
        .and(wm_path("/api/v1/orders"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"id": "o1"}], "next_cursor": null,
        })))
        .mount(&overpay)
        .await;
    Mock::given(wm_method("GET"))
        .and(wm_path_regex(r"^/api/v1/orders/[^/]+$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {"id": "o1", "fulfillment_status": "delivered", "delivered_content": "hi"}
        })))
        .expect(1)
        .mount(&overpay)
        .await;

    let (server, _tmp) = dashboard_with_overpay(&overpay.uri(), "http://owallet.test").await;
    login_admin(&server).await;

    let res = server.post("/wallet/purchases/sync").await;
    res.assert_status(StatusCode::SEE_OTHER);
    let loc = res.header("location");
    let loc = loc.to_str().unwrap();
    assert!(loc.contains("notice=synced"), "loc: {loc}");
    assert!(loc.contains("count=1"), "loc: {loc}");

    // The synced order is now visible in the list.
    let body = server.get("/wallet/purchases").await.text();
    assert!(body.contains("/wallet/purchases/o1"), "body: {body}");

    // A second sync sees the order already cached in a terminal state and
    // skips the per-order detail fetch — the `.expect(1)` on the detail
    // mock (verified on drop) is what proves no second round-trip happened.
    let res = server.post("/wallet/purchases/sync").await;
    res.assert_status(StatusCode::SEE_OTHER);
    let loc = res.header("location");
    let loc = loc.to_str().unwrap();
    assert!(loc.contains("count=0"), "loc: {loc}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn purchases_anonymous_redirects_to_login() {
    let (server, _tmp) = dashboard_with_overpay("http://127.0.0.1:1", "http://owallet.test").await;
    let res = server.get("/wallet/purchases").await;
    res.assert_status(StatusCode::SEE_OTHER);
    assert_eq!(res.header("location").to_str().unwrap(), "/wallet/login");
}
