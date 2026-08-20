//! End-to-end tests for the full router (dashboard + OAuth AS + /mcp).
//!
//! These tests drive the axum router through `axum_test::TestServer`,
//! never opening a real socket, and assert against the MCP JSON-RPC
//! surface and the OAuth AS metadata + token endpoints.

use std::sync::Arc;

use axum::http::StatusCode;
use axum_test::TestServer;
use owallet_db::Database;
use owallet_http::{build_full_router, AppState, EvmConfig};
use owallet_overpay::OverpayClient;
use serde_json::{json, Value};
use tempfile::TempDir;
use wiremock::matchers::{header, method, path, path_regex};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const ISSUER: &str = "http://owallet.test";

fn router(tmp: &TempDir, overpay_uri: &str) -> TestServer {
    router_with_evm(tmp, overpay_uri, EvmConfig::default())
}

fn router_with_evm(tmp: &TempDir, overpay_uri: &str, evm: EvmConfig) -> TestServer {
    let path = tmp.path().join("test.db");
    let db = Database::init(&path, "master-pw").unwrap();
    let overpay = Arc::new(OverpayClient::new(overpay_uri).unwrap());
    let state = AppState::new(db, overpay, evm, ISSUER.to_string());
    let app = build_full_router(state, ISSUER.to_string());
    let mut server = TestServer::new(app).unwrap();
    server.save_cookies();
    server
}

fn seed_abandon_wallet(db_path: &std::path::Path) {
    let mut db = Database::open(db_path).unwrap();
    assert!(db.unlock("master-pw").unwrap());
    db.write_wallet(
        "npub1evm",
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
        Some("0x9858effd232b4033e47d90003d41ec34ecaeda94"),
    )
    .unwrap();
    db.write_default_npub("npub1evm").unwrap();
}

// ---------------------------------------------------------------------------
// OAuth AS metadata + dynamic client registration
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn well_known_metadata_lists_endpoints() {
    let tmp = TempDir::new().unwrap();
    let s = router(&tmp, "http://127.0.0.1:1");
    let res = s.get("/.well-known/oauth-authorization-server").await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["issuer"], ISSUER);
    assert_eq!(
        body["authorization_endpoint"],
        format!("{ISSUER}/oauth/authorize")
    );
    assert_eq!(body["token_endpoint"], format!("{ISSUER}/oauth/token"));
    assert_eq!(body["code_challenge_methods_supported"], json!(["S256"]));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dynamic_registration_returns_client_id() {
    let tmp = TempDir::new().unwrap();
    let s = router(&tmp, "http://127.0.0.1:1");
    let res = s
        .post("/oauth/register")
        .json(&json!({
            "client_name": "test-client",
            "redirect_uris": ["http://127.0.0.1:4444/callback"],
        }))
        .await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert!(body["client_id"].as_str().unwrap().starts_with("client_"));
    assert_eq!(
        body["redirect_uris"],
        json!(["http://127.0.0.1:4444/callback"])
    );
    assert_eq!(body["grant_types"], json!(["authorization_code"]));
}

// ---------------------------------------------------------------------------
// /mcp without auth: tools/list works, but token-gated tools refuse
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_initialize_returns_server_info_and_capabilities() {
    let tmp = TempDir::new().unwrap();
    let s = router(&tmp, "http://127.0.0.1:1");
    let res = s
        .post("/mcp")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        }))
        .await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["jsonrpc"], "2.0");
    assert_eq!(body["id"], 1);
    assert_eq!(body["result"]["serverInfo"]["name"], "owallet");
    assert!(body["result"]["capabilities"]["tools"].is_object());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_tools_list_returns_full_catalog() {
    let tmp = TempDir::new().unwrap();
    let s = router(&tmp, "http://127.0.0.1:1");
    let res = s
        .post("/mcp")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/list",
            "params": {}
        }))
        .await;
    res.assert_status_ok();
    let body: Value = res.json();
    let tools = body["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    for expected in [
        "get_account_info",
        "list_marketplace",
        "get_wallet_orders",
        "create_order",
        "get_order_status",
        "wait_for_order",
        "buy",
        "send_usdc",
        "redeem_merchant_credits",
    ] {
        assert!(names.contains(&expected), "missing tool {expected}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_method_not_found_returns_jsonrpc_error() {
    let tmp = TempDir::new().unwrap();
    let s = router(&tmp, "http://127.0.0.1:1");
    let res = s
        .post("/mcp")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": "abc",
            "method": "nonexistent/method",
            "params": {}
        }))
        .await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["id"], "abc");
    assert_eq!(body["error"]["code"], -32601);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_list_marketplace_calls_overpay() {
    let overpay = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/listings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                {"id": "L1", "title": "Demo", "seller_slug": "alice", "price_usd": 1.23}
            ],
        })))
        .mount(&overpay)
        .await;

    let tmp = TempDir::new().unwrap();
    let s = router(&tmp, &overpay.uri());
    let res = s
        .post("/mcp")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "tools/call",
            "params": {
                "name": "list_marketplace",
                "arguments": { "limit": 5 }
            }
        }))
        .await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["result"]["isError"], false);
    // content is now a rendered summary (not raw JSON); the full payload
    // stays in structuredContent (fathom-x/overpay#295).
    let content_text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(content_text.contains("L1"), "rendered text: {content_text}");
    assert!(
        content_text.contains("Demo"),
        "rendered text: {content_text}"
    );
    assert!(
        content_text.contains("get_listing"),
        "next-step steer: {content_text}"
    );
    assert_eq!(
        body["result"]["structuredContent"]["data"][0]["title"],
        "Demo"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_get_wallet_orders_without_token_falls_back_to_nip98() {
    let overpay = MockServer::start().await;
    // No stored Bearer → the tool must sign via NIP-98 instead of failing.
    // Match the request on the Nostr-flavoured Authorization header to
    // prove the fallback actually fires.
    Mock::given(method("GET"))
        .and(path("/api/v1/orders"))
        .and(wiremock::matchers::header_regex(
            "authorization",
            r"^Nostr .+$",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                {"id": "O1", "payment_status": "paid", "fulfillment_status": "shipping"},
            ],
        })))
        .mount(&overpay)
        .await;

    let tmp = TempDir::new().unwrap();
    let s = router(&tmp, &overpay.uri());
    let path = tmp.path().join("test.db");
    let mut db = Database::open(&path).unwrap();
    assert!(db.unlock("master-pw").unwrap());
    // The abandon mnemonic gives us a deterministic, real secp256k1 key the
    // NIP-98 signer can produce a valid Schnorr signature for.
    db.write_wallet(
        "npub1test",
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
        Some("0xabc"),
    )
    .unwrap();
    db.write_default_npub("npub1test").unwrap();
    drop(db);

    let res = s
        .post("/mcp")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 11,
            "method": "tools/call",
            "params": {
                "name": "get_wallet_orders",
                "arguments": {}
            }
        }))
        .await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["result"]["isError"], false);
    let content_text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(content_text.contains("O1"), "rendered text: {content_text}");
    let data = &body["result"]["structuredContent"]["data"];
    // Sanitized rows use the converged `order_id` key (#391).
    assert_eq!(data[0]["order_id"], "O1");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_get_wallet_orders_with_no_wallet_returns_no_wallet_error() {
    let overpay = MockServer::start().await;
    let tmp = TempDir::new().unwrap();
    let s = router(&tmp, &overpay.uri());

    let res = s
        .post("/mcp")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 11,
            "method": "tools/call",
            "params": {
                "name": "get_wallet_orders",
                "arguments": {}
            }
        }))
        .await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["result"]["isError"], true);
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("no wallet selected"), "got: {text}");
    // Friendly errors append an actionable next step (#295).
    assert!(text.contains("owallet select"), "next-step hint: {text}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_send_usdc_rejects_bad_recipient_address() {
    let tmp = TempDir::new().unwrap();
    // Send_usdc needs a wallet to derive the signing key.
    let path = tmp.path().join("test.db");
    let db = Database::init(&path, "master-pw").unwrap();
    let overpay = Arc::new(OverpayClient::new("http://127.0.0.1:1").unwrap());
    let state = AppState::new(db, overpay, EvmConfig::default(), ISSUER.to_string());
    let app = build_full_router(state, ISSUER.to_string());
    let mut s = TestServer::new(app).unwrap();
    s.save_cookies();
    seed_abandon_wallet(&path);

    let res = s
        .post("/mcp")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 12,
            "method": "tools/call",
            "params": {
                "name": "send_usdc",
                "arguments": { "to": "not-an-address", "amount": 1.0 }
            }
        }))
        .await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["result"]["isError"], true);
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("invalid recipient address"), "got: {text}");
    assert!(text.contains("Next:"), "next-step hint: {text}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_send_usdc_rejects_unknown_chain() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("test.db");
    let db = Database::init(&path, "master-pw").unwrap();
    let overpay = Arc::new(OverpayClient::new("http://127.0.0.1:1").unwrap());
    let evm = EvmConfig {
        rpc_url: "http://127.0.0.1:1".into(),
        network: "eip155:999999".into(),
    };
    let state = AppState::new(db, overpay, evm, ISSUER.to_string());
    let app = build_full_router(state, ISSUER.to_string());
    let mut s = TestServer::new(app).unwrap();
    s.save_cookies();
    seed_abandon_wallet(&path);

    let res = s
        .post("/mcp")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 12,
            "method": "tools/call",
            "params": {
                "name": "send_usdc",
                "arguments": { "to": "0x000000000000000000000000000000000000dead", "amount": 1.0 }
            }
        }))
        .await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["result"]["isError"], true);
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("unsupported chain"), "got: {text}");
    assert!(text.contains("Next:"), "next-step hint: {text}");
}

// ---------------------------------------------------------------------------
// /mcp with bearer (full token-issuance flow goes via /consent → /oauth/token)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_bearer_unknown_token_returns_401() {
    let tmp = TempDir::new().unwrap();
    let s = router(&tmp, "http://127.0.0.1:1");
    let res = s
        .post("/mcp")
        .add_header("authorization", "Bearer not_a_real_token")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 21,
            "method": "tools/list",
            "params": {}
        }))
        .await;
    res.assert_status(StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_bearer_known_token_unlocks_wallet_scoped_tools() {
    let overpay = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/account"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {"username": "alice", "formatted_account_number": "1234-5678-9012-3456"},
        })))
        .mount(&overpay)
        .await;

    let tmp = TempDir::new().unwrap();
    let s = router(&tmp, &overpay.uri());
    let path = tmp.path().join("test.db");
    let mut db = Database::open(&path).unwrap();
    assert!(db.unlock("master-pw").unwrap());
    db.write_wallet(
        "npub1alice",
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
        Some("0xabc"),
    )
    .unwrap();
    db.write_default_npub("npub1alice").unwrap();
    // Filed under the Overpay host, exactly as `owallet authorize` files it.
    // The MCP tools derive the same key from their Overpay client, so a
    // CLI-linked wallet is usable here without re-authorizing.
    db.write_token(
        "npub1alice",
        &owallet_overpay::host_key(&overpay.uri()),
        "the_overpay_bearer",
        "overpay-oauth",
    )
    .unwrap();
    // Issue a local MCP token via the access_tokens table directly.
    db.write_access_token(
        "mcp_tok",
        "mcp_client",
        &["wallet".into()],
        None,
        Some("npub1alice"),
    )
    .unwrap();
    drop(db);

    let res = s
        .post("/mcp")
        .add_header("authorization", "Bearer mcp_tok")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 31,
            "method": "tools/call",
            "params": {
                "name": "get_account_info",
                "arguments": {}
            }
        }))
        .await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["result"]["isError"], false);
    // `get_account_info` emits a single markdown summary block in
    // `content`; the structured payload lives in `structuredContent`
    // (fathom-x/overpay#295). Both legs are sanitized of on-chain and
    // identity data before they leave the machine (fathom-x/overpay#391):
    // the username survives, the npub / EVM address / pubkey do not.
    let dump = &body["result"]["structuredContent"];
    assert_eq!(dump["account"]["data"]["username"], "alice");
    assert!(
        dump.get("npub").is_none() && dump.get("address").is_none(),
        "identity fields must not leave the machine: {dump}"
    );
    let serialized = body["result"].to_string();
    assert!(
        !serialized.contains("npub1alice") && !serialized.contains("0xabc"),
        "no response leg may carry wallet identity: {serialized}"
    );
    let md = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(md.contains("| Username | alice |"), "markdown: {md}");
}

/// Builds before the host-key fix filed the Overpay bearer under the local
/// dashboard's issuer URL. Those rows have to keep working — read through to
/// the legacy key and re-file, rather than 401 until the user re-authorizes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_bearer_finds_token_filed_under_legacy_issuer_key() {
    let overpay = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/account"))
        .and(header("authorization", "Bearer legacy_bearer"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {"username": "legacy-alice"},
        })))
        .mount(&overpay)
        .await;

    let tmp = TempDir::new().unwrap();
    let s = router(&tmp, &overpay.uri());
    let path = tmp.path().join("test.db");
    let mut db = Database::open(&path).unwrap();
    assert!(db.unlock("master-pw").unwrap());
    db.write_wallet(
        "npub1alice",
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
        Some("0xabc"),
    )
    .unwrap();
    db.write_default_npub("npub1alice").unwrap();
    db.write_token("npub1alice", ISSUER, "legacy_bearer", "overpay-oauth")
        .unwrap();
    db.write_access_token(
        "mcp_tok",
        "mcp_client",
        &["wallet".into()],
        None,
        Some("npub1alice"),
    )
    .unwrap();
    drop(db);

    let res = s
        .post("/mcp")
        .add_header("authorization", "Bearer mcp_tok")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 32,
            "method": "tools/call",
            "params": {"name": "get_account_info", "arguments": {}}
        }))
        .await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["result"]["isError"], false);
    assert_eq!(
        body["result"]["structuredContent"]["account"]["data"]["username"],
        "legacy-alice"
    );

    // The row moved to the canonical key, so the next lookup is a direct hit.
    let mut db = Database::open(&path).unwrap();
    assert!(db.unlock("master-pw").unwrap());
    let canonical = owallet_overpay::host_key(&overpay.uri());
    assert_eq!(
        db.read_token("npub1alice", &canonical).unwrap().as_deref(),
        Some("legacy_bearer")
    );
    assert_eq!(db.read_token("npub1alice", ISSUER).unwrap(), None);
}

// ---------------------------------------------------------------------------
// /consent page
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn consent_get_with_unknown_session_400() {
    let tmp = TempDir::new().unwrap();
    let s = router(&tmp, "http://127.0.0.1:1");
    let res = s.get("/consent").add_query_param("session", "bogus").await;
    res.assert_status(StatusCode::BAD_REQUEST);
    assert!(res.text().contains("Session expired"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authorize_redirects_to_consent_for_a_known_client() {
    let tmp = TempDir::new().unwrap();
    let s = router(&tmp, "http://127.0.0.1:1");

    // Register a client.
    let reg = s
        .post("/oauth/register")
        .json(&json!({
            "redirect_uris": ["http://127.0.0.1:5555/cb"]
        }))
        .await;
    reg.assert_status_ok();
    let client_id = reg.json::<Value>()["client_id"]
        .as_str()
        .unwrap()
        .to_string();

    let res = s
        .get("/oauth/authorize")
        .add_query_param("response_type", "code")
        .add_query_param("client_id", client_id.as_str())
        .add_query_param("redirect_uri", "http://127.0.0.1:5555/cb")
        .add_query_param("code_challenge", "fakechallenge")
        .add_query_param("code_challenge_method", "S256")
        .add_query_param("state", "xyz")
        .add_query_param("scope", "wallet")
        .await;
    res.assert_status(StatusCode::SEE_OTHER);
    let loc_hdr = res.header("location");
    let loc = loc_hdr.to_str().unwrap();
    assert!(loc.starts_with("/consent?session="), "got {loc}");
}

/// The whole browser-auth path for the OpenAI-compatible endpoint: an OAuth
/// client requesting `scope=provider` gets an `owk_` provider key from the
/// code exchange (not an `at_` MCP token), bound to the consented wallet,
/// visible in the dashboard's revocation list, and accepted by `/v1`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_scope_flow_mints_a_revocable_provider_key() {
    let tmp = TempDir::new().unwrap();
    let s = router(&tmp, "http://127.0.0.1:1");

    // Seed a wallet with a per-wallet password (consent verifies it).
    let db_path = tmp.path().join("test.db");
    {
        let mut db = Database::open(&db_path).unwrap();
        assert!(db.unlock("master-pw").unwrap());
        db.write_wallet(
            "npub1evm",
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
            None,
        )
        .unwrap();
        db.write_default_npub("npub1evm").unwrap();
        db.write_wallet_password("npub1evm", "wallet-pw").unwrap();
    }

    let reg = s
        .post("/oauth/register")
        .json(&json!({"redirect_uris": ["http://127.0.0.1:5555/cb"]}))
        .await;
    reg.assert_status_ok();
    let client_id = reg.json::<Value>()["client_id"]
        .as_str()
        .unwrap()
        .to_string();

    let verifier = "provider-scope-test-verifier-0123456789abcdef";
    let challenge = {
        use sha2::{Digest, Sha256};
        b64url(Sha256::digest(verifier.as_bytes()).as_slice())
    };

    let res = s
        .get("/oauth/authorize")
        .add_query_param("response_type", "code")
        .add_query_param("client_id", client_id.as_str())
        .add_query_param("redirect_uri", "http://127.0.0.1:5555/cb")
        .add_query_param("code_challenge", challenge.as_str())
        .add_query_param("code_challenge_method", "S256")
        .add_query_param("scope", "provider")
        .await;
    res.assert_status(StatusCode::SEE_OTHER);
    let loc_hdr = res.header("location");
    let session = loc_hdr
        .to_str()
        .unwrap()
        .strip_prefix("/consent?session=")
        .unwrap()
        .to_string();

    // The consent page says what a provider-scope approval creates.
    let page = s.get("/consent").add_query_param("session", &session).await;
    page.assert_status_ok();
    assert!(page.text().contains("provider API key"));

    let res = s
        .post("/consent")
        .form(&json!({
            "session": session,
            "npub": "npub1evm",
            "password": "wallet-pw",
            "action": "approve",
        }))
        .await;
    res.assert_status(StatusCode::SEE_OTHER);
    let loc_hdr = res.header("location");
    let loc = loc_hdr.to_str().unwrap();
    let code = loc
        .split(['?', '&'])
        .find_map(|kv| kv.strip_prefix("code="))
        .unwrap_or_else(|| panic!("no code in redirect '{loc}'"))
        .to_string();

    let res = s
        .post("/oauth/token")
        .form(&json!({
            "grant_type": "authorization_code",
            "code": code,
            "redirect_uri": "http://127.0.0.1:5555/cb",
            "client_id": client_id,
            "code_verifier": verifier,
        }))
        .await;
    res.assert_status_ok();
    let key = res.json::<Value>()["access_token"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        key.starts_with("owk_"),
        "expected provider key, got '{key}'"
    );

    // Bound to the consented wallet and listed for dashboard revocation.
    {
        let mut db = Database::open(&db_path).unwrap();
        assert!(db.unlock("master-pw").unwrap());
        assert_eq!(
            db.read_provider_key_npub(&key).unwrap().as_deref(),
            Some("npub1evm")
        );
        let listed = db.list_provider_keys("npub1evm").unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].label.as_deref(), Some("browser login"));
        assert_eq!(listed[0].token_prefix.as_deref(), Some(&key[..12]));
        assert!(
            !listed[0].can_spend(),
            "a browser login without the spend checkbox must mint a chat-only key"
        );
    }

    // /v1 accepts the minted key (anything but 401 proves auth passed —
    // the model list itself 500s here because Overpay is unreachable) and
    // still rejects an unknown one.
    let ok = s
        .get("/v1/models")
        .add_header("authorization", format!("Bearer {key}"))
        .await;
    assert_ne!(ok.status_code(), StatusCode::UNAUTHORIZED);
    let bad = s
        .get("/v1/models")
        .add_header("authorization", "Bearer owk_bogus")
        .await;
    bad.assert_status(StatusCode::UNAUTHORIZED);
}

/// Drive register → authorize(`scope`) → consent (optionally ticking the
/// spend checkbox and filling the budget field) → PKCE token exchange,
/// returning the minted credential. The wallet `npub1evm` (password
/// `wallet-pw`) must already be seeded.
async fn browser_login_flow(
    s: &axum_test::TestServer,
    scope: &str,
    tick_allow_spend: bool,
    spend_budget_usd: Option<&str>,
) -> String {
    let reg = s
        .post("/oauth/register")
        .json(&json!({"redirect_uris": ["http://127.0.0.1:5555/cb"]}))
        .await;
    reg.assert_status_ok();
    let client_id = reg.json::<Value>()["client_id"]
        .as_str()
        .unwrap()
        .to_string();

    let verifier = "consent-scope-test-verifier-0123456789abcdef";
    let challenge = {
        use sha2::{Digest, Sha256};
        b64url(Sha256::digest(verifier.as_bytes()).as_slice())
    };

    let res = s
        .get("/oauth/authorize")
        .add_query_param("response_type", "code")
        .add_query_param("client_id", client_id.as_str())
        .add_query_param("redirect_uri", "http://127.0.0.1:5555/cb")
        .add_query_param("code_challenge", challenge.as_str())
        .add_query_param("code_challenge_method", "S256")
        .add_query_param("scope", scope)
        .await;
    res.assert_status(StatusCode::SEE_OTHER);
    let loc_hdr = res.header("location");
    let session = loc_hdr
        .to_str()
        .unwrap()
        .strip_prefix("/consent?session=")
        .unwrap()
        .to_string();

    let mut form = json!({
        "session": session,
        "npub": "npub1evm",
        "password": "wallet-pw",
        "action": "approve",
    });
    if tick_allow_spend {
        form["allow_spend"] = json!("on");
    }
    if let Some(budget) = spend_budget_usd {
        form["spend_budget_usd"] = json!(budget);
    }
    let res = s.post("/consent").form(&form).await;
    res.assert_status(StatusCode::SEE_OTHER);
    let loc_hdr = res.header("location");
    let loc = loc_hdr.to_str().unwrap();
    let code = loc
        .split(['?', '&'])
        .find_map(|kv| kv.strip_prefix("code="))
        .unwrap_or_else(|| panic!("no code in redirect '{loc}'"))
        .to_string();

    let res = s
        .post("/oauth/token")
        .form(&json!({
            "grant_type": "authorization_code",
            "code": code,
            "redirect_uri": "http://127.0.0.1:5555/cb",
            "client_id": client_id,
            "code_verifier": verifier,
        }))
        .await;
    res.assert_status_ok();
    res.json::<Value>()["access_token"]
        .as_str()
        .unwrap()
        .to_string()
}

/// The consent checkbox — and nothing else — decides whether a
/// browser-minted provider key can spend: ticking it grants the scope,
/// while a client *requesting* `scope=provider spend` without the user's
/// tick still gets a chat-only key.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn consent_checkbox_controls_the_minted_keys_spend_scope() {
    let tmp = TempDir::new().unwrap();
    let s = router(&tmp, "http://127.0.0.1:1");

    let db_path = tmp.path().join("test.db");
    {
        let mut db = Database::open(&db_path).unwrap();
        assert!(db.unlock("master-pw").unwrap());
        db.write_wallet(
            "npub1evm",
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
            None,
        )
        .unwrap();
        db.write_default_npub("npub1evm").unwrap();
        db.write_wallet_password("npub1evm", "wallet-pw").unwrap();
    }

    // The consent page offers the spending choice for provider scope.
    // (Ticked path first so the key order below is deterministic.)
    let spend_key = browser_login_flow(&s, "provider", true, None).await;
    let sneaky_key = browser_login_flow(&s, "provider spend", false, None).await;

    let mut db = Database::open(&db_path).unwrap();
    assert!(db.unlock("master-pw").unwrap());

    let auth = db.read_provider_key_auth(&spend_key).unwrap().unwrap();
    assert!(
        auth.can_spend(),
        "ticked checkbox must grant spend, got scopes {:?}",
        auth.scopes
    );
    assert_eq!(
        auth.daily_budget_usd_cents, None,
        "blank budget field must mint a no-limit key"
    );

    let auth = db.read_provider_key_auth(&sneaky_key).unwrap().unwrap();
    assert!(
        !auth.can_spend(),
        "client-requested spend scope without the user's tick must be stripped, got {:?}",
        auth.scopes
    );
}

/// The consent page's budget field rides the auth code to token exchange:
/// a browser-minted spend key carries the user's chosen lifetime budget.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn consent_budget_field_bounds_the_browser_minted_key() {
    let tmp = TempDir::new().unwrap();
    let s = router(&tmp, "http://127.0.0.1:1");

    let db_path = tmp.path().join("test.db");
    {
        let mut db = Database::open(&db_path).unwrap();
        assert!(db.unlock("master-pw").unwrap());
        db.write_wallet(
            "npub1evm",
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
            None,
        )
        .unwrap();
        db.write_default_npub("npub1evm").unwrap();
        db.write_wallet_password("npub1evm", "wallet-pw").unwrap();
    }

    let key = browser_login_flow(&s, "provider", true, Some("$12.50")).await;

    let mut db = Database::open(&db_path).unwrap();
    assert!(db.unlock("master-pw").unwrap());
    let auth = db.read_provider_key_auth(&key).unwrap().unwrap();
    assert!(auth.can_spend());
    assert_eq!(
        auth.daily_budget_usd_cents,
        Some(1250),
        "the consent budget must land on the minted key"
    );
    assert_eq!(auth.spent_today_usd_cents(), 0);
}

/// Tiny base64url(NO_PAD) encoder for the PKCE challenge — mirrors the
/// private `base64_url` in `oauth_as.rs`.
fn b64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i)) as usize & 0x3f] as char);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// MCP tools: merchant credits + buy (formerly stubbed, now wired)
// ---------------------------------------------------------------------------

/// Same shape as the `JsonRpcMock` in `owallet-evm/tests/send_test.rs`.
/// Deliberately duplicated — the obvious DRY move (extract to
/// `owallet-evm::test_support` behind a feature gate) breaks
/// `cargo test --workspace` because the gated integration test silently
/// skips on the default feature set. Two 50-line copies is the lesser
/// evil. Keep them in sync if you tweak the canned responses.
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
            // ERC-20 balanceOf(address) → 5_000_000 = 5 USDC.
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

/// Set up a router whose Overpay base URL is `overpay.uri()` and whose
/// EVM RPC URL is `rpc.uri()`. The wallet seeded into the DB is the
/// canonical abandon-mnemonic so the buy-tool send_usdc step has a real
/// signing key to derive from.
async fn router_with_overpay_and_rpc(
    tmp: &TempDir,
    overpay_uri: &str,
    rpc_uri: &str,
) -> TestServer {
    let server = router_with_evm(
        tmp,
        overpay_uri,
        EvmConfig {
            rpc_url: rpc_uri.to_string(),
            network: "eip155:8453".into(),
        },
    );
    seed_abandon_wallet(&tmp.path().join("test.db"));
    server
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_get_account_info_includes_onchain_balances() {
    let overpay = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/account"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {"username": "alice", "formatted_account_number": "0001-0002-0003-0004"},
        })))
        .mount(&overpay)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/merchant_credits"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                {"seller_slug": "core", "balance_cents": 500, "formatted_balance": "$5.00"},
            ],
        })))
        .mount(&overpay)
        .await;
    let rpc = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(JsonRpcMock)
        .mount(&rpc)
        .await;

    let tmp = TempDir::new().unwrap();
    let s = router_with_overpay_and_rpc(&tmp, &overpay.uri(), &rpc.uri()).await;

    let res = s
        .post("/mcp")
        .json(&json!({
            "jsonrpc": "2.0", "id": 1,
            "method": "tools/call",
            "params": {"name": "get_account_info", "arguments": {}}
        }))
        .await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["result"]["isError"], false);
    // `get_account_info` now emits a single markdown summary block in
    // `content` plus the structured payload in `structuredContent`
    // (fathom-x/overpay#295). The markdown rows match the Python tool's
    // table layout.
    let content = body["result"]["content"].as_array().unwrap();
    assert_eq!(content.len(), 1, "expected one markdown block: {content:?}");
    let md = content[0]["text"].as_str().unwrap();
    assert!(
        md.starts_with("| Field | Value |"),
        "markdown table missing: {md}"
    );
    assert!(md.contains("| Network | eip155:8453 |"));
    assert!(md.contains("| ETH Balance | 1 ETH (Base) |"));
    assert!(md.contains("| USDC Balance | 5 USDC (Base) |"));
    assert!(md.contains("| Username | alice |"));
    // The account number, pubkey, and addresses stay on the dashboard/CLI
    // (fathom-x/overpay#391).
    assert!(
        !md.contains("0001-0002-0003-0004"),
        "account number must not leave the machine: {md}"
    );

    // Sanitized shape (fathom-x/overpay#391): flat formatted balances,
    // projected credits, no identity fields.
    let dump = &body["result"]["structuredContent"];
    assert_eq!(dump["network"], "eip155:8453");
    assert_eq!(dump["chain_id"], 8453);
    assert!(dump.get("pubkey").is_none() && dump.get("address").is_none());
    assert_eq!(dump["eth_balance"], "1");
    assert_eq!(dump["usdc_balance"], "5");
    assert_eq!(dump["account"]["data"]["username"], "alice");
    assert_eq!(dump["merchant_credits"][0]["seller_slug"], "core");
    assert_eq!(dump["merchant_credits"][0]["balance_cents"], 500);
    assert!(md.contains("@core"), "credits in markdown: {md}");
    assert!(md.contains("$5.00"), "credits in markdown: {md}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_redeem_merchant_credits_posts_order_id() {
    let overpay = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path_regex(
            r"^/api/v1/merchant_credits/[A-Za-z0-9_-]+/redeem$",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "applied",
            "amount_redeemed_cents": 1500,
            "credit_balance_cents": 3500,
            "message": "Applied $15.00",
        })))
        .mount(&overpay)
        .await;
    let tmp = TempDir::new().unwrap();
    let s = router_with_overpay_and_rpc(&tmp, &overpay.uri(), "http://127.0.0.1:1").await;

    let res = s
        .post("/mcp")
        .json(&json!({
            "jsonrpc": "2.0", "id": 1,
            "method": "tools/call",
            "params": {"name": "redeem_merchant_credits", "arguments": {
                "seller_slug": "alice", "order_id": "ord_xyz"
            }}
        }))
        .await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["result"]["isError"], false);
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("$15.00"), "redeemed amount: {text}");
    assert!(text.contains("$35.00"), "remaining balance: {text}");
    assert_eq!(body["result"]["structuredContent"]["status"], "applied");
    assert_eq!(
        body["result"]["structuredContent"]["amount_redeemed_cents"],
        1500
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_buy_composes_purchase_then_send_usdc() {
    // Overpay side: respond to the credit-purchase POST with a payment address.
    let overpay = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path_regex(
            r"^/api/v1/merchant_credits/[A-Za-z0-9_-]+/purchase$",
        ))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "order_id": "ord_buy_001",
            "payment_address": "0x000000000000000000000000000000000000dead",
            "payment_amount_usdc": 7.50,
            "order_url": "https://overpay.com/orders/ord_buy_001",
        })))
        .mount(&overpay)
        .await;

    // EVM RPC side: the same JsonRpcMock the send_usdc tests use.
    let rpc = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(JsonRpcMock)
        .mount(&rpc)
        .await;

    let tmp = TempDir::new().unwrap();
    let s = router_with_overpay_and_rpc(&tmp, &overpay.uri(), &rpc.uri()).await;

    let res = s
        .post("/mcp")
        .json(&json!({
            "jsonrpc": "2.0", "id": 1,
            "method": "tools/call",
            "params": {"name": "buy", "arguments": {
                "seller_slug": "alice", "amount_usd": 7.50
            }}
        }))
        .await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(
        body["result"]["isError"], false,
        "buy returned error: {}",
        body["result"]["content"][0]["text"]
    );
    let structured = &body["result"]["structuredContent"];
    // The Python tool's strict six-field shape (`server.py:2172-2179`)
    // minus the tx hash, which the sanitized MCP surface withholds
    // (fathom-x/overpay#391). `payment_address`, `chain`, `block_number`,
    // `seller_slug`, `explorer_url` remain intentionally dropped.
    assert_eq!(structured["order_id"], "ord_buy_001");
    assert_eq!(structured["payment_amount_usdc"], 7.50);
    assert_eq!(structured["status"], "payment_sent");
    assert!(structured["note"].as_str().unwrap().contains("Credits"));
    assert!(structured["order_url"].is_string());
    // The sanitized MCP surface never returns the tx hash — it stays on
    // the dashboard/CLI (fathom-x/overpay#391).
    assert!(
        structured.get("tx_hash").is_none(),
        "tx_hash must NOT leave the machine: {structured:?}"
    );
    assert!(
        structured.get("payment_address").is_none(),
        "payment_address must NOT be in the output: {structured:?}",
    );
    assert!(
        structured.get("chain").is_none(),
        "chain must NOT be in the strict-shape output: {structured:?}",
    );
    // Rendered text confirms the send + steers to wait_for_order (#295).
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("ord_buy_001"), "rendered text: {text}");
    assert!(text.contains("Payment sent"), "rendered text: {text}");
    assert!(text.contains("wait_for_order"), "next-step steer: {text}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_buy_rejects_non_positive_amount() {
    // No mocks needed; the amount is validated before either backend is hit.
    let tmp = TempDir::new().unwrap();
    let s = router_with_overpay_and_rpc(&tmp, "http://127.0.0.1:1", "http://127.0.0.1:1").await;
    let res = s
        .post("/mcp")
        .json(&json!({
            "jsonrpc": "2.0", "id": 1,
            "method": "tools/call",
            "params": {"name": "buy", "arguments": {
                "seller_slug": "alice", "amount_usd": 0.0
            }}
        }))
        .await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["result"]["isError"], true);
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("positive"), "got: {text}");
}

// Keep the `header` matcher import-used.
#[allow(dead_code)]
fn _ensure_header_matcher_used() {
    let _ = header("authorization", "Bearer x");
}

// ---------------------------------------------------------------------------
// get_listing + create_order buyer_note schema validation
// ---------------------------------------------------------------------------

/// Mount `GET /api/v1/listings/{id}` returning the supplied Rails body.
async fn mount_listing(server: &MockServer, body: Value) {
    Mock::given(method("GET"))
        .and(path_regex(r"^/api/v1/listings/[^/]+$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_get_listing_forwards_rails_envelope_with_schema() {
    let overpay = MockServer::start().await;
    mount_listing(
        &overpay,
        json!({
            "data": {
                "id": "L42",
                "title": "Run Python Code",
                "buyer_note_schema": {
                    "type": "object",
                    "title": "Python execution request",
                    "required": ["code"],
                    "properties": {
                        "code": {"type": "string", "description": "Python source to execute"}
                    }
                }
            }
        }),
    )
    .await;
    let tmp = TempDir::new().unwrap();
    let s = router_with_overpay_and_rpc(&tmp, &overpay.uri(), "http://127.0.0.1:1").await;

    let res = s
        .post("/mcp")
        .json(&json!({
            "jsonrpc": "2.0", "id": 1,
            "method": "tools/call",
            "params": {"name": "get_listing", "arguments": {"listing_id": "L42"}}
        }))
        .await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["result"]["isError"], false);
    // Verbatim forward — Rails envelope intact, schema round-tripped.
    let structured = &body["result"]["structuredContent"];
    // Sanitized listings use the converged `listing_id` key (#391).
    assert_eq!(structured["data"]["listing_id"], "L42");
    assert_eq!(
        structured["data"]["buyer_note_schema"]["required"][0],
        "code"
    );
    // Rendered text names the required field + steers to create_order (#295).
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("L42"), "rendered text: {text}");
    assert!(text.contains("code"), "required field surfaced: {text}");
    assert!(text.contains("create_order"), "steer: {text}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_create_order_validates_buyer_note_against_schema() {
    let overpay = MockServer::start().await;
    mount_listing(
        &overpay,
        json!({
            "data": {
                "id": "L42",
                "buyer_note_schema": {
                    "type": "object",
                    "title": "Python execution request",
                    "required": ["code"],
                    "properties": {"code": {"type": "string"}}
                }
            }
        }),
    )
    .await;
    let tmp = TempDir::new().unwrap();
    let s = router_with_overpay_and_rpc(&tmp, &overpay.uri(), "http://127.0.0.1:1").await;

    let res = s
        .post("/mcp")
        .json(&json!({
            "jsonrpc": "2.0", "id": 1,
            "method": "tools/call",
            "params": {
                "name": "create_order",
                "arguments": {"listing_id": "L42", "buyer_note": {}}
            }
        }))
        .await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["result"]["isError"], true, "body: {body}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("code") && text.contains("required"),
        "error text should name the missing required field; got: {text}"
    );
    assert!(
        text.contains("Python execution request"),
        "error text should include the schema title; got: {text}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_create_order_accepts_valid_schema_compliant_note() {
    use wiremock::matchers::body_partial_json;
    let overpay = MockServer::start().await;
    mount_listing(
        &overpay,
        json!({
            "data": {
                "id": "L42",
                "buyer_note_schema": {
                    "type": "object",
                    "required": ["code"],
                    "properties": {"code": {"type": "string"}}
                }
            }
        }),
    )
    .await;
    // Asserts the wire body sends buyer_note as a JSON-encoded *string*
    // (Python parity: `server.py:1924`'s `create_order` is typed
    // `Optional[str]` and bot fulfillment `JSON.parse`s it). The MCP
    // layer accepts an object from the caller and stringifies it before
    // submission, so the bot's JSON.parse still works.
    Mock::given(method("POST"))
        .and(path("/api/v1/orders"))
        .and(body_partial_json(json!({
            "listing_id": "L42",
            "buyer_note": "{\"code\":\"print(1+1)\"}"
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "data": {"id": "O1", "payment_status": "pending"}
        })))
        .mount(&overpay)
        .await;

    let tmp = TempDir::new().unwrap();
    let s = router_with_overpay_and_rpc(&tmp, &overpay.uri(), "http://127.0.0.1:1").await;

    let res = s
        .post("/mcp")
        .json(&json!({
            "jsonrpc": "2.0", "id": 1,
            "method": "tools/call",
            "params": {
                "name": "create_order",
                "arguments": {
                    "listing_id": "L42",
                    "buyer_note": {"code": "print(1+1)"}
                }
            }
        }))
        .await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["result"]["isError"], false, "body: {body}");
    assert_eq!(
        body["result"]["structuredContent"]["data"]["order_id"],
        "O1"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_create_order_accepts_json_encoded_string_buyer_note() {
    // Python parity: Python's `create_order` is typed `Optional[str]`, and
    // the cross-impl E2E `test_e2e_mcp_buy_flow.py` passes
    // `buyer_note=json.dumps({"code": ...})` — i.e. a JSON-encoded
    // *string* against an object schema. Our pre-flight validator must
    // parse the string before validating, so it doesn't reject the
    // string-of-an-object against `type: object`.
    use wiremock::matchers::body_partial_json;
    let overpay = MockServer::start().await;
    mount_listing(
        &overpay,
        json!({
            "data": {
                "id": "L42",
                "buyer_note_schema": {
                    "type": "object",
                    "required": ["code"],
                    "properties": {"code": {"type": "string"}}
                }
            }
        }),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/orders"))
        .and(body_partial_json(json!({
            "listing_id": "L42",
            // Wire body keeps the caller's string verbatim.
            "buyer_note": "{\"code\": \"print(1+1)\"}"
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "data": {"id": "O2", "payment_status": "pending"}
        })))
        .mount(&overpay)
        .await;

    let tmp = TempDir::new().unwrap();
    let s = router_with_overpay_and_rpc(&tmp, &overpay.uri(), "http://127.0.0.1:1").await;

    let res = s
        .post("/mcp")
        .json(&json!({
            "jsonrpc": "2.0", "id": 1,
            "method": "tools/call",
            "params": {
                "name": "create_order",
                "arguments": {
                    "listing_id": "L42",
                    "buyer_note": "{\"code\": \"print(1+1)\"}"
                }
            }
        }))
        .await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["result"]["isError"], false, "body: {body}");
    assert_eq!(
        body["result"]["structuredContent"]["data"]["order_id"],
        "O2"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_create_order_passes_through_when_no_schema() {
    let overpay = MockServer::start().await;
    mount_listing(
        &overpay,
        json!({"data": {"id": "L42", "buyer_note_schema": null}}),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/orders"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "data": {"id": "O2", "payment_status": "pending"}
        })))
        .mount(&overpay)
        .await;
    let tmp = TempDir::new().unwrap();
    let s = router_with_overpay_and_rpc(&tmp, &overpay.uri(), "http://127.0.0.1:1").await;

    let res = s
        .post("/mcp")
        .json(&json!({
            "jsonrpc": "2.0", "id": 1,
            "method": "tools/call",
            "params": {
                "name": "create_order",
                "arguments": {
                    "listing_id": "L42",
                    "buyer_note": "just some free-form text"
                }
            }
        }))
        .await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["result"]["isError"], false, "body: {body}");
    assert_eq!(
        body["result"]["structuredContent"]["data"]["order_id"],
        "O2"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_create_order_passes_through_on_listing_fetch_failure() {
    // Listing fetch returns 500 → tool should still submit (best-effort)
    // rather than block on a Rails outage.
    let overpay = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/api/v1/listings/[^/]+$"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&overpay)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/orders"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "data": {"id": "O3", "payment_status": "pending"}
        })))
        .mount(&overpay)
        .await;
    let tmp = TempDir::new().unwrap();
    let s = router_with_overpay_and_rpc(&tmp, &overpay.uri(), "http://127.0.0.1:1").await;

    let res = s
        .post("/mcp")
        .json(&json!({
            "jsonrpc": "2.0", "id": 1,
            "method": "tools/call",
            "params": {
                "name": "create_order",
                "arguments": {"listing_id": "L42", "buyer_note": "anything"}
            }
        }))
        .await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["result"]["isError"], false, "body: {body}");
    assert_eq!(
        body["result"]["structuredContent"]["data"]["order_id"],
        "O3"
    );
}

// ---------------------------------------------------------------------------
// Parity locks: every tool below was rewritten to byte-match the Python
// `wallet_mcp/server.py` output shape (fathom-x/overpay#288 follow-up).
// These tests are guard-rails to keep that contract green.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_wait_for_order_returns_snap_plus_waited_seconds_and_timed_out_false() {
    let overpay = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/api/v1/orders/[A-Za-z0-9_-]+$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "id": "ord_wait_1", "fulfillment_status": "delivered" },
        })))
        .mount(&overpay)
        .await;

    let tmp = TempDir::new().unwrap();
    let s = router(&tmp, &overpay.uri());
    // The abandon mnemonic gives us a deterministic real secp256k1 key
    // so the NIP-98 fallback in resolve_owned_auth() works.
    let path = tmp.path().join("test.db");
    let mut db = Database::open(&path).unwrap();
    assert!(db.unlock("master-pw").unwrap());
    db.write_wallet(
        "npub1abandon",
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
        Some("0xabc"),
    )
    .unwrap();
    db.write_default_npub("npub1abandon").unwrap();
    drop(db);

    let res = s
        .post("/mcp")
        .json(&json!({
            "jsonrpc": "2.0", "id": 1,
            "method": "tools/call",
            "params": {
                "name": "wait_for_order",
                "arguments": {
                    "order_id": "ord_wait_1",
                    "timeout_seconds": 5,
                    "poll_interval_seconds": 1,
                }
            }
        }))
        .await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["result"]["isError"], false);
    let snap = &body["result"]["structuredContent"];
    // Rails envelope preserved + Python's extra fields spliced on top
    // (server.py:2045).
    assert_eq!(snap["data"]["order_id"], "ord_wait_1");
    assert_eq!(snap["data"]["fulfillment_status"], "delivered");
    assert!(
        snap["waited_seconds"].is_number(),
        "waited_seconds missing: {snap}"
    );
    assert_eq!(snap["timed_out"], false);
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("ord_wait_1"), "rendered text: {text}");
    assert!(text.contains("Reached after"), "rendered text: {text}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_wait_for_order_timeout_returns_snap_with_timed_out_true() {
    let overpay = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/api/v1/orders/[A-Za-z0-9_-]+$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "id": "ord_wait_2", "fulfillment_status": "shipping" },
        })))
        .mount(&overpay)
        .await;

    let tmp = TempDir::new().unwrap();
    let s = router(&tmp, &overpay.uri());
    let path = tmp.path().join("test.db");
    let mut db = Database::open(&path).unwrap();
    assert!(db.unlock("master-pw").unwrap());
    db.write_wallet(
        "npub1abandon",
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
        Some("0xabc"),
    )
    .unwrap();
    db.write_default_npub("npub1abandon").unwrap();
    drop(db);

    let res = s
        .post("/mcp")
        .json(&json!({
            "jsonrpc": "2.0", "id": 1,
            "method": "tools/call",
            "params": {
                "name": "wait_for_order",
                "arguments": {
                    "order_id": "ord_wait_2",
                    // Loop exits at the first iteration because
                    // `elapsed + poll >= timeout` (1s + 1s >= 1s).
                    "timeout_seconds": 1,
                    "poll_interval_seconds": 1,
                }
            }
        }))
        .await;
    res.assert_status_ok();
    let body: Value = res.json();
    // Python returns the snap (with `timed_out: true`) instead of
    // raising on timeout — server.py:2046-2047.
    assert_eq!(body["result"]["isError"], false);
    let snap = &body["result"]["structuredContent"];
    assert_eq!(snap["data"]["fulfillment_status"], "shipping");
    assert_eq!(snap["timed_out"], true);
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("Timed out after"), "rendered text: {text}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_buy_no_usdc_seller_returns_error_dict_not_iserror() {
    let overpay = MockServer::start().await;
    // Purchase succeeds but Rails omits payment_address / payment_amount_usdc
    // because the seller doesn't have a USDC wallet. Python returns this
    // as a partial-success `{error, order_id, order_url, hint}` dict at
    // `server.py:2154-2160` — NOT a hard MCP error.
    Mock::given(method("POST"))
        .and(path_regex(
            r"^/api/v1/merchant_credits/[A-Za-z0-9_-]+/purchase$",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "order_id":  "ord_partial",
                "order_url": "https://example.com/orders/ord_partial",
            },
        })))
        .mount(&overpay)
        .await;

    let tmp = TempDir::new().unwrap();
    let s = router(&tmp, &overpay.uri());
    let path = tmp.path().join("test.db");
    let mut db = Database::open(&path).unwrap();
    assert!(db.unlock("master-pw").unwrap());
    db.write_wallet(
        "npub1abandon",
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
        Some("0xabc"),
    )
    .unwrap();
    db.write_default_npub("npub1abandon").unwrap();
    drop(db);

    let res = s
        .post("/mcp")
        .json(&json!({
            "jsonrpc": "2.0", "id": 1,
            "method": "tools/call",
            "params": {
                "name": "buy",
                "arguments": { "seller_slug": "non-usdc-seller", "amount_usd": 5.00 }
            }
        }))
        .await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["result"]["isError"], false);
    let r = &body["result"]["structuredContent"];
    assert!(r["error"].as_str().unwrap().contains("USDC wallet"));
    assert_eq!(r["order_id"], "ord_partial");
    assert_eq!(r["order_url"], "https://example.com/orders/ord_partial");
    assert!(r["hint"].as_str().unwrap().contains("web checkout"));
    // Soft error renders as a warning with the order_url surfaced (#295).
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("⚠️"), "warning marker: {text}");
    assert!(
        text.contains("https://example.com/orders/ord_partial"),
        "order_url surfaced: {text}"
    );
}

// ---------------------------------------------------------------------------
// Purchase cache: get_order_status caching/stripping + the new tools
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_get_order_status_caches_terminal_and_strips_large_content() {
    let overpay = MockServer::start().await;
    let big = "X".repeat(3000);
    Mock::given(method("GET"))
        .and(path_regex(r"^/api/v1/orders/[^/]+$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "id": "ord1",
                "fulfillment_status": "delivered",
                "delivered_content": big,
                "delivered_content_type": "text/plain",
            }
        })))
        .mount(&overpay)
        .await;
    let tmp = TempDir::new().unwrap();
    let s = router_with_overpay_and_rpc(&tmp, &overpay.uri(), "http://127.0.0.1:1").await;

    // get_order_status strips the >2KB delivered_content to a pointer.
    let res = s
        .post("/mcp")
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "get_order_status", "arguments": {"order_id": "ord1"}}
        }))
        .await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["result"]["isError"], false, "body: {body}");
    let data = &body["result"]["structuredContent"]["data"];
    assert!(
        data["delivered_content"].is_null(),
        "content should be stripped"
    );
    assert_eq!(data["delivered_content_cached"]["size_bytes"], 3000);
    assert!(data["delivered_content_cached"]["hint"]
        .as_str()
        .unwrap()
        .contains("get_purchase"));
    // Rendered text surfaces the cached-content pointer + steer (#295).
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("3000"), "cached size in text: {text}");
    assert!(text.contains("get_purchase"), "steer: {text}");

    // get_purchase returns the full cached content.
    let res2 = s
        .post("/mcp")
        .json(&json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": "get_purchase", "arguments": {"order_id": "ord1"}}
        }))
        .await;
    let body2: Value = res2.json();
    assert_eq!(body2["result"]["isError"], false);
    assert_eq!(
        body2["result"]["structuredContent"]["delivered_content"]
            .as_str()
            .unwrap()
            .len(),
        3000
    );

    // list_purchases omits the heavy fields.
    let res3 = s
        .post("/mcp")
        .json(&json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "list_purchases", "arguments": {}}
        }))
        .await;
    let body3: Value = res3.json();
    let purchases = &body3["result"]["structuredContent"]["purchases"];
    assert_eq!(body3["result"]["structuredContent"]["count"], 1);
    assert_eq!(purchases[0]["order_id"], "ord1");
    assert!(purchases[0].get("delivered_content").is_none());
    assert!(purchases[0].get("snapshot").is_none());
    let text3 = body3["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text3.contains("1 cached"), "rendered text: {text3}");
    assert!(text3.contains("ord1"), "rendered text: {text3}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_get_order_status_include_delivered_content_keeps_it() {
    let overpay = MockServer::start().await;
    let big = "Y".repeat(3000);
    Mock::given(method("GET"))
        .and(path_regex(r"^/api/v1/orders/[^/]+$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {"id": "ord2", "fulfillment_status": "delivered", "delivered_content": big}
        })))
        .mount(&overpay)
        .await;
    let tmp = TempDir::new().unwrap();
    let s = router_with_overpay_and_rpc(&tmp, &overpay.uri(), "http://127.0.0.1:1").await;

    let res = s
        .post("/mcp")
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "get_order_status",
                       "arguments": {"order_id": "ord2", "include_delivered_content": true}}
        }))
        .await;
    let body: Value = res.json();
    let data = &body["result"]["structuredContent"]["data"];
    assert_eq!(data["delivered_content"].as_str().unwrap().len(), 3000);
    assert!(data["delivered_content_cached"].is_null());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_get_purchase_returns_not_cached_for_unknown() {
    let tmp = TempDir::new().unwrap();
    let s = router_with_overpay_and_rpc(&tmp, "http://127.0.0.1:1", "http://127.0.0.1:1").await;
    let res = s
        .post("/mcp")
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "get_purchase", "arguments": {"order_id": "nope"}}
        }))
        .await;
    let body: Value = res.json();
    assert_eq!(body["result"]["structuredContent"]["error"], "not_cached");
    assert_eq!(body["result"]["structuredContent"]["order_id"], "nope");
    // not_cached renders as an info nudge steering to get_order_status (#295).
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("nope"), "rendered text: {text}");
    assert!(text.contains("get_order_status"), "steer: {text}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_sync_purchases_backfills_from_rails() {
    let overpay = MockServer::start().await;
    // Orders list (delivered) — one page, no cursor.
    Mock::given(method("GET"))
        .and(path("/api/v1/orders"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"id": "o1"}, {"id": "o2"}],
            "next_cursor": null,
        })))
        .mount(&overpay)
        .await;
    // Per-order detail.
    Mock::given(method("GET"))
        .and(path_regex(r"^/api/v1/orders/[^/]+$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {"id": "ox", "fulfillment_status": "delivered", "delivered_content": "hi"}
        })))
        .mount(&overpay)
        .await;
    let tmp = TempDir::new().unwrap();
    let s = router_with_overpay_and_rpc(&tmp, &overpay.uri(), "http://127.0.0.1:1").await;

    let res = s
        .post("/mcp")
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "sync_purchases", "arguments": {}}
        }))
        .await;
    let body: Value = res.json();
    assert_eq!(body["result"]["isError"], false, "body: {body}");
    assert_eq!(body["result"]["structuredContent"]["synced"], 2);
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("Synced 2"), "rendered text: {text}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_list_marketplace_flattens_delivery_eta() {
    let overpay = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/listings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                {"id": "L1", "delivery_eta": {"p50_seconds": 8, "p90_seconds": 16}},
                {"id": "L2"},
            ],
            "next_cursor": null,
        })))
        .mount(&overpay)
        .await;
    let tmp = TempDir::new().unwrap();
    let s = router_with_overpay_and_rpc(&tmp, &overpay.uri(), "http://127.0.0.1:1").await;

    let res = s
        .post("/mcp")
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "list_marketplace", "arguments": {}}
        }))
        .await;
    let body: Value = res.json();
    let data = &body["result"]["structuredContent"]["data"];
    assert_eq!(data[0]["delivery_eta_seconds"], 8);
    assert!(
        data[0].get("delivery_eta").is_none(),
        "delivery_eta object should be flattened away"
    );
    // Listing without an eta gets a null delivery_eta_seconds.
    assert!(data[1]["delivery_eta_seconds"].is_null());
}

// ---------------------------------------------------------------------------
// Streamable-HTTP: `tools/call` over Server-Sent Events
// ---------------------------------------------------------------------------

/// A `tools/call` from a client that accepts `text/event-stream` is
/// answered as an SSE stream whose final event carries the JSON-RPC
/// response. Proves content negotiation + final-event framing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_tools_call_streams_over_sse_when_accepted() {
    let overpay = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/listings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                {"id": "L9", "title": "Streamed", "seller_slug": "alice", "price_usd": 1.0}
            ],
        })))
        .mount(&overpay)
        .await;

    let tmp = TempDir::new().unwrap();
    let s = router(&tmp, &overpay.uri());
    let res = s
        .post("/mcp")
        .add_header("accept", "text/event-stream")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 42,
            "method": "tools/call",
            "params": { "name": "list_marketplace", "arguments": { "limit": 5 } }
        }))
        .await;

    res.assert_status_ok();
    let ct = res
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        ct.starts_with("text/event-stream"),
        "expected an SSE response, got content-type {ct:?}"
    );

    // The stream's terminal event is the JSON-RPC response, framed as an
    // SSE `data:` line. Pull it back out and assert on it.
    let text = res.text();
    let resp = parse_last_sse_json(&text);
    assert_eq!(resp["jsonrpc"], "2.0");
    assert_eq!(resp["id"], 42);
    assert_eq!(resp["result"]["isError"], false);
    assert_eq!(
        resp["result"]["structuredContent"]["data"][0]["title"],
        "Streamed"
    );
}

/// A `tools/call` that keeps the order in flight for one poll streams a
/// `notifications/progress` event before the final response. Exercises the
/// full progress path end to end with `wait_for_order`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_wait_for_order_streams_progress_then_result() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    // First poll: still shipping. Second poll: delivered → return.
    struct SeqOrder {
        calls: AtomicUsize,
    }
    impl Respond for SeqOrder {
        fn respond(&self, _req: &Request) -> ResponseTemplate {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let status = if n == 0 { "shipping" } else { "delivered" };
            ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "O9",
                    "payment_status": "paid",
                    "fulfillment_status": status,
                }
            }))
        }
    }

    let overpay = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/orders/O9"))
        .respond_with(SeqOrder {
            calls: AtomicUsize::new(0),
        })
        .mount(&overpay)
        .await;

    let tmp = TempDir::new().unwrap();
    let s = router(&tmp, &overpay.uri());
    seed_abandon_wallet(&tmp.path().join("test.db"));

    let res = s
        .post("/mcp")
        .add_header("accept", "text/event-stream")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 43,
            "method": "tools/call",
            "params": {
                "name": "wait_for_order",
                "arguments": {
                    "order_id": "O9",
                    "until_status": "delivered",
                    "timeout_seconds": 30,
                    "poll_interval_seconds": 1
                },
                "_meta": { "progressToken": "p9" }
            }
        }))
        .await;

    res.assert_status_ok();
    let text = res.text();

    // At least one progress notification, tagged with our token.
    assert!(
        text.contains("notifications/progress"),
        "expected a progress notification in the stream:\n{text}"
    );
    assert!(
        text.contains("\"progressToken\":\"p9\""),
        "progress notification should echo the client's token:\n{text}"
    );

    // Terminal event is the final result with the delivered order.
    let resp = parse_last_sse_json(&text);
    assert_eq!(resp["id"], 43);
    assert_eq!(resp["result"]["isError"], false);
    assert_eq!(
        resp["result"]["structuredContent"]["data"]["fulfillment_status"],
        "delivered"
    );
}

/// The end of the streaming path: a seller publishing its work in progress
/// shows up as token deltas on the buyer's SSE stream, each carrying only
/// what is new since the previous notification.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_wait_for_order_streams_seller_output_as_deltas() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    // The seller's buffer grows across polls, then the order delivers.
    struct GrowingBuffer {
        calls: AtomicUsize,
    }
    impl Respond for GrowingBuffer {
        fn respond(&self, _req: &Request) -> ResponseTemplate {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let body = match n {
                0 => json!({"data": {
                    "id": "O10", "payment_status": "paid",
                    "fulfillment_status": "processing",
                    "partial_content": "Four score", "partial_seq": 1,
                }}),
                1 => json!({"data": {
                    "id": "O10", "payment_status": "paid",
                    "fulfillment_status": "processing",
                    "partial_content": "Four score and seven", "partial_seq": 2,
                }}),
                _ => json!({"data": {
                    "id": "O10", "payment_status": "paid",
                    "fulfillment_status": "delivered",
                    "delivered_content": "Four score and seven years ago",
                }}),
            };
            ResponseTemplate::new(200).set_body_json(body)
        }
    }

    let overpay = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/orders/O10"))
        .respond_with(GrowingBuffer {
            calls: AtomicUsize::new(0),
        })
        .mount(&overpay)
        .await;

    let tmp = TempDir::new().unwrap();
    let s = router(&tmp, &overpay.uri());
    seed_abandon_wallet(&tmp.path().join("test.db"));

    let res = s
        .post("/mcp")
        .add_header("accept", "text/event-stream")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 44,
            "method": "tools/call",
            "params": {
                "name": "wait_for_order",
                "arguments": {
                    "order_id": "O10",
                    "timeout_seconds": 30,
                    "poll_interval_seconds": 1
                },
                "_meta": { "progressToken": "p10" }
            }
        }))
        .await;

    res.assert_status_ok();
    let text = res.text();

    let deltas: Vec<String> = text
        .lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
        .filter(|v| v["method"] == "notifications/progress")
        .filter_map(|v| {
            v["params"]["data"]["delta"]
                .as_str()
                .map(std::string::ToString::to_string)
        })
        .collect();

    assert_eq!(
        deltas,
        vec!["Four score".to_string(), " and seven".to_string()],
        "each notification should carry only the newly generated text:\n{text}"
    );

    // The delivered payload is still the whole answer, not just the tail.
    let resp = parse_last_sse_json(&text);
    assert_eq!(resp["id"], 44);
    assert_eq!(
        resp["result"]["structuredContent"]["data"]["delivered_content"],
        "Four score and seven years ago"
    );
}

/// Pull the JSON payload out of the last `data:` line of an SSE body.
fn parse_last_sse_json(body: &str) -> Value {
    let last = body
        .lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .map(|l| l.trim())
        .rfind(|l| !l.is_empty())
        .unwrap_or_else(|| panic!("no SSE data line in body:\n{body}"));
    serde_json::from_str(last).unwrap_or_else(|e| panic!("bad SSE json {last:?}: {e}"))
}

// ---------------------------------------------------------------------------
// Provider keys as /mcp credentials (client-side tools work)
// ---------------------------------------------------------------------------

/// An `owk_` provider key doubles as an /mcp bearer: it binds the session
/// to the key's wallet, and the key's scopes govern the money tools — a
/// chat-scoped key is refused spending with the scope message, before any
/// marketplace traffic.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_accepts_a_provider_key_bearer_and_enforces_its_scopes() {
    let overpay = MockServer::start().await;
    let tmp = TempDir::new().unwrap();
    let s = router(&tmp, &overpay.uri());
    seed_abandon_wallet(&tmp.path().join("test.db"));
    let (chat_key_secret, spend_key_secret) = {
        let mut db = Database::open(&tmp.path().join("test.db")).unwrap();
        assert!(db.unlock("master-pw").unwrap());
        let chat = db
            .create_provider_key("npub1evm", "chat-only", "chat", None)
            .unwrap()
            .1;
        let spend = db
            .create_provider_key("npub1evm", "spender", "chat spend", None)
            .unwrap()
            .1;
        (chat, spend)
    };

    // A bogus owk_ bearer is rejected outright.
    let bad = s
        .post("/mcp")
        .add_header("authorization", "Bearer owk_bogus")
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}))
        .await;
    bad.assert_status(StatusCode::UNAUTHORIZED);

    // A real key authenticates.
    let ok = s
        .post("/mcp")
        .add_header("authorization", format!("Bearer {chat_key_secret}"))
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}))
        .await;
    ok.assert_status_ok();

    // Chat-scoped key: pay_order refuses on scope (no Overpay mock is
    // mounted for orders — a network call would surface differently).
    let refused = s
        .post("/mcp")
        .add_header("authorization", format!("Bearer {chat_key_secret}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": "pay_order", "arguments": {"order_id": "ORD-1"}}
        }))
        .await;
    refused.assert_status_ok();
    let body: Value = refused.json();
    let text = body.to_string();
    assert!(
        text.contains("chat-scoped"),
        "scope refusal expected: {text}"
    );

    // Spend-scoped key: raw-address sends stay refused regardless.
    let sends = s
        .post("/mcp")
        .add_header("authorization", format!("Bearer {spend_key_secret}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "send_usdc", "arguments": {"to": "0x1", "amount": "1"}}
        }))
        .await;
    sends.assert_status_ok();
    let body: Value = sends.json();
    let text = body.to_string();
    assert!(text.contains("dashboard"), "send refusal expected: {text}");
}
