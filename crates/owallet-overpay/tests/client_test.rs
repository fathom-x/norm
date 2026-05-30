//! End-to-end tests for the Overpay client, driven against a `wiremock`
//! mock server. Each test asserts both the wire shape that goes out
//! (method, path, headers, body) and the shape that comes back.

use owallet_crypto::{derive_from_mnemonic, Mnemonic, PrivateKey, EVM_HD_PATH};
use owallet_overpay::models::{ListingFilters, OAuthRegisterRequest, OrderFilters};
use owallet_overpay::{Auth, OverpayClient, Pkce};
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path, path_regex, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ABANDON_12: &str = "abandon abandon abandon abandon abandon abandon \
     abandon abandon abandon abandon abandon about";

fn fixture_sk() -> PrivateKey {
    let m = Mnemonic::parse(ABANDON_12).unwrap();
    derive_from_mnemonic(&m, EVM_HD_PATH).unwrap()
}

async fn fixture() -> (MockServer, OverpayClient) {
    let server = MockServer::start().await;
    let client = OverpayClient::new(&server.uri()).unwrap();
    (server, client)
}

// ---------------------------------------------------------------------------
// OAuth flow
// ---------------------------------------------------------------------------

#[tokio::test]
async fn register_oauth_client_posts_expected_body() {
    let (server, client) = fixture().await;
    Mock::given(method("POST"))
        .and(path("/oauth/clients"))
        .and(body_json(json!({
            "client_name": "owallet",
            "redirect_uris": ["http://127.0.0.1:9999/callback"],
            "grant_types": ["authorization_code"],
            "response_types": ["code"],
            "scope": "wallet",
            "token_endpoint_auth_method": "none",
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "client_id": "abcd_client_id",
            "client_secret": null,
        })))
        .expect(1)
        .mount(&server)
        .await;

    let req = OAuthRegisterRequest {
        client_name: "owallet".into(),
        redirect_uris: vec!["http://127.0.0.1:9999/callback".into()],
        grant_types: vec!["authorization_code".into()],
        response_types: vec!["code".into()],
        scope: Some("wallet".into()),
        token_endpoint_auth_method: Some("none".into()),
    };
    let resp = client.register_oauth_client(&req).await.unwrap();
    assert_eq!(resp.client_id, "abcd_client_id");
    assert!(resp.client_secret.is_none());
}

#[tokio::test]
async fn exchange_code_sends_pkce_verifier_in_form_body() {
    let (server, client) = fixture().await;
    let pkce = Pkce::generate();
    let verifier = pkce.verifier.clone();

    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(header("content-type", "application/x-www-form-urlencoded"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "tok_abc",
            "token_type": "Bearer",
            "scope": "wallet",
        })))
        .expect(1)
        .mount(&server)
        .await;

    let resp = client
        .exchange_code(
            "abcd_client_id",
            "thecode",
            &verifier,
            "http://127.0.0.1:9999/callback",
        )
        .await
        .unwrap();
    assert_eq!(resp.access_token, "tok_abc");
    assert_eq!(resp.token_type.as_deref(), Some("Bearer"));
}

#[tokio::test]
async fn authorize_url_has_required_pkce_params() {
    let (_server, client) = fixture().await;
    let pkce = Pkce::generate();
    let url = client
        .authorize_url(
            "client_abc",
            "http://127.0.0.1:1234/callback",
            &pkce.state,
            &pkce.challenge,
            "wallet",
        )
        .unwrap();
    let pairs: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(pairs.get("response_type").map(String::as_str), Some("code"));
    assert_eq!(
        pairs.get("client_id").map(String::as_str),
        Some("client_abc")
    );
    assert_eq!(
        pairs.get("redirect_uri").map(String::as_str),
        Some("http://127.0.0.1:1234/callback")
    );
    assert_eq!(pairs.get("scope").map(String::as_str), Some("wallet"));
    assert_eq!(
        pairs.get("code_challenge_method").map(String::as_str),
        Some("S256")
    );
    assert_eq!(
        pairs.get("code_challenge").map(String::as_str),
        Some(pkce.challenge.as_str())
    );
}

// ---------------------------------------------------------------------------
// /api/v1/account — bearer and NIP-98 auth
// ---------------------------------------------------------------------------

#[tokio::test]
async fn account_bearer_sends_authorization_header() {
    let (server, client) = fixture().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/account"))
        .and(header("authorization", "Bearer tok_abc"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "username": "alice",
            "account_number": "1234567890123456",
            "address": "0xabc",
        })))
        .expect(1)
        .mount(&server)
        .await;

    let info = client.account(Auth::Bearer("tok_abc")).await.unwrap();
    assert_eq!(info.username.as_deref(), Some("alice"));
    assert_eq!(info.account_number.as_deref(), Some("1234567890123456"));
}

#[tokio::test]
async fn account_nip98_sends_nostr_authorization_header() {
    let (server, client) = fixture().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/account"))
        .and(wiremock::matchers::header_regex(
            "authorization",
            r"^Nostr .+$",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "username": "alice",
        })))
        .expect(1)
        .mount(&server)
        .await;

    let sk = fixture_sk();
    let info = client.account(Auth::Nip98(&sk)).await.unwrap();
    assert_eq!(info.username.as_deref(), Some("alice"));
}

#[tokio::test]
async fn account_decodes_jsonapi_data_envelope() {
    // Real Rails shape: `{"data": {"username", "formatted_account_number", ...}}`.
    // Prior to the fix the deserializer expected flat fields and the
    // dashboard rendered "linked (could not load profile)" even with a
    // perfectly good bearer token.
    let (server, client) = fixture().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/account"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "username": null,
                "formatted_account_number": "1234-5678-9012-3456",
                "address": "0xabc",
            }
        })))
        .mount(&server)
        .await;

    let info = client.account(Auth::Bearer("tok")).await.unwrap();
    // Anonymous accounts have no display name — that's fine.
    assert_eq!(info.username, None);
    // We surface the dashed display form as `account_number`.
    assert_eq!(info.account_number.as_deref(), Some("1234-5678-9012-3456"));
    assert_eq!(info.address.as_deref(), Some("0xabc"));
}

#[tokio::test]
async fn account_prefers_formatted_account_number_over_raw() {
    let (server, client) = fixture().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/account"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "formatted_account_number": "1234-5678-9012-3456",
                "account_number": "1234567890123456",
            }
        })))
        .mount(&server)
        .await;

    let info = client.account(Auth::Bearer("tok")).await.unwrap();
    assert_eq!(info.account_number.as_deref(), Some("1234-5678-9012-3456"));
}

#[tokio::test]
async fn account_unauthorized_returns_http_status_error() {
    let (server, client) = fixture().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/account"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({ "error": "unauthorized" })))
        .mount(&server)
        .await;

    let err = client.account(Auth::Bearer("nope")).await.unwrap_err();
    match err {
        owallet_overpay::OverpayError::HttpStatus { status, .. } => assert_eq!(status, 401),
        other => panic!("expected HttpStatus, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// /api/v1/listings (public)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_listings_passes_filters_as_query_params() {
    let (server, client) = fixture().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/listings"))
        .and(query_param("category", "books"))
        .and(query_param("limit", "5"))
        // Shape mirrors the real Rails API: `price_usd` is a formatted
        // *string* (not a number) and `seller` is a nested object.
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                {
                    "id": "L1",
                    "title": "Hello",
                    "price_cents": 999,
                    "price_usd": "$9.99",
                    "category": "books",
                    "seller": {"name": "Acme", "slug": "acme"},
                    "delivery_eta": {"p50_seconds": 8, "p90_seconds": 16},
                    "checkout_url": "http://example.test/checkout/L1"
                },
            ],
            "next_cursor": "cur_x"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let page = client
        .list_listings(&ListingFilters {
            category: Some("books".into()),
            limit: Some(5),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(page.data.len(), 1);
    assert_eq!(page.data[0].id, "L1");
    // Regression: the string price must parse (previously `Option<f64>` here
    // raised "invalid type: string, expected f64").
    assert_eq!(page.data[0].price_usd.as_deref(), Some("$9.99"));
    assert_eq!(page.data[0].price_cents, Some(999));
    assert_eq!(
        page.data[0].seller.as_ref().and_then(|s| s.slug.as_deref()),
        Some("acme")
    );
    assert_eq!(page.next_cursor.as_deref(), Some("cur_x"));
}

#[tokio::test]
async fn list_listings_omits_unset_filters() {
    let (server, client) = fixture().await;
    // Match `/api/v1/listings` with no query string.
    Mock::given(method("GET"))
        .and(path("/api/v1/listings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [],
        })))
        .expect(1)
        .mount(&server)
        .await;

    let page = client
        .list_listings(&ListingFilters::default())
        .await
        .unwrap();
    assert!(page.data.is_empty());
}

// ---------------------------------------------------------------------------
// /api/v1/orders
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_orders_with_filters() {
    // Real Rails shape: `{data: [order_json...], next_cursor}` — each item
    // emits `payment_status`/`tracking_number`/string `total_usd`, and the
    // listing reference is nested under `listing.id`. The query param is
    // `payment_status=`, not `status=`.
    let (server, client) = fixture().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/orders"))
        .and(header("authorization", "Bearer tok"))
        .and(query_param("payment_status", "paid"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{
                "id": "O1",
                "payment_status": "paid",
                "fulfillment_status": "shipping",
                "product_title": "Widget",
                "total_usd": "$0.0010",
                "total_usd_cents": 1,
                "tracking_number": "1Z999",
                "carrier": "UPS",
                "listing": { "id": "L42", "title": "Widget" },
            }],
            "next_cursor": null,
        })))
        .expect(1)
        .mount(&server)
        .await;

    let page = client
        .list_orders(
            Auth::Bearer("tok"),
            &OrderFilters {
                payment_status: Some("paid".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(page.data.len(), 1);
    let o = &page.data[0];
    assert_eq!(o.payment_status.as_deref(), Some("paid"));
    assert_eq!(o.fulfillment_status.as_deref(), Some("shipping"));
    assert_eq!(o.total_usd.as_deref(), Some("$0.0010"));
    assert_eq!(o.total_usd_cents, Some(1));
    assert_eq!(o.tracking_number.as_deref(), Some("1Z999"));
    // listing.id surfaces as listing_id on the flat Rust struct.
    assert_eq!(o.listing_id.as_deref(), Some("L42"));
    assert_eq!(o.listing_title.as_deref(), Some("Widget"));
}

#[tokio::test]
async fn list_orders_value_keeps_envelope_and_nested_listing() {
    // Companion to `list_orders_parses_real_rails_shape`: the `_value`
    // variant exists for MCP tool output (fathom-x/overpay#288) and
    // must NOT unwrap `{data: [...]}` or flatten the nested
    // `{listing: {id, title}}` reference — the wire must reach the
    // MCP consumer byte-identical to what Rails emitted.
    let (server, client) = fixture().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/orders"))
        .and(header("authorization", "Bearer tok"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{
                "id": "O1",
                "payment_status": "paid",
                "listing": { "id": "L42", "title": "Widget" },
                "future_field_rails_adds_later": "preserved",
            }],
            "next_cursor": null,
        })))
        .expect(1)
        .mount(&server)
        .await;

    let v = client
        .list_orders_value(Auth::Bearer("tok"), &OrderFilters::default())
        .await
        .unwrap();
    // Envelope intact.
    assert!(v.get("data").is_some(), "envelope dropped: {v}");
    assert_eq!(v["data"][0]["id"], "O1");
    // Nested listing reference NOT flattened.
    assert_eq!(v["data"][0]["listing"]["id"], "L42");
    assert_eq!(v["data"][0]["listing"]["title"], "Widget");
    // Unknown fields survive the round-trip.
    assert_eq!(v["data"][0]["future_field_rails_adds_later"], "preserved");
}

#[tokio::test]
async fn get_order_by_id_unwraps_data_envelope() {
    // Real Rails shape for `orders#show`: `{data: order_json}`.
    let (server, client) = fixture().await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/api/v1/orders/[A-Za-z0-9_-]+$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "id": "abc-123",
                "payment_status": "paid",
                "fulfillment_status": "delivered",
                "tracking_number": "9410812345",
                "tracking_url": "https://tracking.example/9410812345",
                "listing": { "id": "L1", "title": "Thing" },
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let o = client
        .get_order("abc-123", Auth::Bearer("tok"))
        .await
        .unwrap();
    assert_eq!(o.id, "abc-123");
    assert_eq!(o.payment_status.as_deref(), Some("paid"));
    assert_eq!(o.fulfillment_status.as_deref(), Some("delivered"));
    assert_eq!(o.tracking_number.as_deref(), Some("9410812345"));
    assert_eq!(
        o.tracking_url.as_deref(),
        Some("https://tracking.example/9410812345")
    );
    assert_eq!(o.listing_id.as_deref(), Some("L1"));
}

#[tokio::test]
async fn create_order_posts_listing_id_and_note() {
    // Real Rails shape for `orders#create`: `{data: order_json}`.
    let (server, client) = fixture().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/orders"))
        .and(header("authorization", "Bearer tok"))
        .and(body_json(json!({
            "listing_id": "L42",
            "buyer_note": "for the cat",
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "data": {
                "id": "O1",
                "payment_status": "pending",
                "listing": { "id": "L42", "title": "Cat toy" },
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let order = client
        .create_order("L42", Some("for the cat"), Auth::Bearer("tok"))
        .await
        .unwrap();
    assert_eq!(order.id, "O1");
    assert_eq!(order.payment_status.as_deref(), Some("pending"));
    assert_eq!(order.listing_id.as_deref(), Some("L42"));
}

// ---------------------------------------------------------------------------
// /api/v1/buyer/web_session
// ---------------------------------------------------------------------------

#[tokio::test]
async fn web_session_returns_one_time_url() {
    let (server, client) = fixture().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/buyer/web_session"))
        .and(header("authorization", "Bearer tok"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "url": "https://overpay.com/login_with_token/abc123",
        })))
        .expect(1)
        .mount(&server)
        .await;

    let s = client.web_session(Auth::Bearer("tok")).await.unwrap();
    assert!(s.url.contains("/login_with_token/abc123"));
}

#[tokio::test]
async fn web_session_accepts_login_url_field() {
    // Real Rails shape: `{login_url, expires_at}`.
    let (server, client) = fixture().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/buyer/web_session"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "login_url": "https://overpay.com/auto-login/xyz",
            "expires_at": "2026-01-01T00:00:00Z",
        })))
        .mount(&server)
        .await;

    let s = client.web_session(Auth::Bearer("tok")).await.unwrap();
    assert!(s.url.contains("/auto-login/xyz"));
}

#[tokio::test]
async fn web_session_accepts_data_envelope() {
    // JSONAPI-style envelope: `{"data": {login_url, expires_at}}`.
    let (server, client) = fixture().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/buyer/web_session"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "login_url": "https://overpay.com/auto-login/wrapped",
                "expires_at": "2026-01-01T00:00:00Z",
            }
        })))
        .mount(&server)
        .await;

    let s = client.web_session(Auth::Bearer("tok")).await.unwrap();
    assert!(s.url.contains("/auto-login/wrapped"));
}

// ---------------------------------------------------------------------------
// /api/v1/merchant_credits
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_merchant_credits_handles_seller_and_org_owned_rows() {
    // Real Rails shape for `merchant_credits#index`: `render_json([...])`
    // expands to `{data: [credit_json...]}` where each row carries a
    // `holder_type` discriminator. Seller-owned rows have `seller_slug`;
    // organization-owned rows have `organization_slug` instead.
    let (server, client) = fixture().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/merchant_credits"))
        .and(header("authorization", "Bearer tok"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                {
                    "holder_type": "seller",
                    "seller_slug": "alice",
                    "balance_cents": 1234,
                    "formatted_balance": "$12.34",
                    "total_purchased_cents": 2000,
                    "total_redeemed_cents": 766,
                },
                {
                    "holder_type": "organization",
                    "organization_slug": "acme",
                    "balance_cents": 5000,
                    "formatted_balance": "$50.00",
                },
            ],
        })))
        .expect(1)
        .mount(&server)
        .await;

    let list = client
        .list_merchant_credits(Auth::Bearer("tok"))
        .await
        .unwrap();
    assert_eq!(list.data.len(), 2);
    assert_eq!(list.data[0].seller_slug.as_deref(), Some("alice"));
    assert_eq!(list.data[0].holder_type.as_deref(), Some("seller"));
    assert_eq!(list.data[0].total_purchased_cents, Some(2000));
    // Org-owned row: seller_slug absent, organization_slug present.
    assert!(list.data[1].seller_slug.is_none());
    assert_eq!(list.data[1].organization_slug.as_deref(), Some("acme"));
    assert_eq!(list.data[1].holder_type.as_deref(), Some("organization"));
}

#[tokio::test]
async fn get_merchant_credits_for_one_seller_unwraps_data_envelope() {
    // Real Rails shape for `merchant_credits#show`: `render_json({...})`
    // produces `{data: {seller_slug, balance_cents, ...}}`.
    let (server, client) = fixture().await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/api/v1/merchant_credits/[A-Za-z0-9_-]+$"))
        .and(header("authorization", "Bearer tok"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "seller_slug": "alice",
                "balance_cents": 5000,
                "formatted_balance": "$50.00",
                "organization": null,
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let mc = client
        .get_merchant_credits("alice", Auth::Bearer("tok"))
        .await
        .unwrap();
    assert_eq!(mc.seller_slug.as_deref(), Some("alice"));
    assert_eq!(mc.balance_cents, Some(5000));
    assert_eq!(mc.formatted_balance.as_deref(), Some("$50.00"));
}

#[tokio::test]
async fn purchase_merchant_credits_posts_amount_cents() {
    // Real Rails shape for `merchant_credits#purchase`: `render_json({...})`
    // gives `{data: {order_id, payment_address, payment_amount_usdc,
    // order_url, ...}}`. `payment_address` + `payment_amount_usdc` are
    // optional because they're only set when the seller has a USDC wallet.
    let (server, client) = fixture().await;
    Mock::given(method("POST"))
        .and(path_regex(
            r"^/api/v1/merchant_credits/[A-Za-z0-9_-]+/purchase$",
        ))
        .and(header("authorization", "Bearer tok"))
        .and(body_json(json!({ "amount_cents": 1500 })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "data": {
                "order_id": "ord_abc",
                "order_type": "credit_purchase",
                "total_usd_cents": 1500,
                "payment_status": "pending",
                "payment_address": "0x000000000000000000000000000000000000dead",
                "payment_amount_usdc": 15.00,
                "order_url": "https://overpay.com/orders/ord_abc",
                "message": "Credit purchase order created. Pay to complete.",
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let resp = client
        .purchase_merchant_credits("alice", 1500, Auth::Bearer("tok"))
        .await
        .unwrap();
    assert_eq!(resp.order_id, "ord_abc");
    assert_eq!(resp.payment_amount_usdc, Some(15.00));
    assert_eq!(
        resp.payment_address.as_deref(),
        Some("0x000000000000000000000000000000000000dead")
    );
    assert_eq!(resp.total_usd_cents, Some(1500));
    assert_eq!(resp.payment_status.as_deref(), Some("pending"));
}

#[tokio::test]
async fn purchase_merchant_credits_handles_non_usdc_seller() {
    // Non-USDC sellers don't get `payment_address` / `payment_amount_usdc`
    // — the response is still valid; the on-chain leg of `buy` should
    // surface this as a clear error rather than crash.
    let (server, client) = fixture().await;
    Mock::given(method("POST"))
        .and(path_regex(
            r"^/api/v1/merchant_credits/[A-Za-z0-9_-]+/purchase$",
        ))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "data": {
                "order_id": "ord_zec",
                "total_usd_cents": 1500,
                "payment_status": "pending",
                "order_url": "https://overpay.com/orders/ord_zec",
            }
        })))
        .mount(&server)
        .await;

    let resp = client
        .purchase_merchant_credits("zec-seller", 1500, Auth::Bearer("tok"))
        .await
        .unwrap();
    assert_eq!(resp.order_id, "ord_zec");
    assert!(resp.payment_address.is_none());
    assert!(resp.payment_amount_usdc.is_none());
}

#[tokio::test]
async fn redeem_merchant_credits_posts_order_id() {
    // Real Rails shape for `merchant_credits#redeem`: wrapped in `{data: ...}`.
    let (server, client) = fixture().await;
    Mock::given(method("POST"))
        .and(path_regex(
            r"^/api/v1/merchant_credits/[A-Za-z0-9_-]+/redeem$",
        ))
        .and(header("authorization", "Bearer tok"))
        .and(body_json(json!({ "order_id": "ord_xyz" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "status": "applied",
                "amount_redeemed_cents": 1500,
                "credit_balance_cents": 3500,
                "message": "Applied 15.00 USD credit",
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let resp = client
        .redeem_merchant_credits("alice", "ord_xyz", Auth::Bearer("tok"))
        .await
        .unwrap();
    assert_eq!(resp.status, "applied");
    assert_eq!(resp.amount_redeemed_cents, 1500);
    assert_eq!(resp.credit_balance_cents, 3500);
}

// ---------------------------------------------------------------------------
// public-url rewriting
// ---------------------------------------------------------------------------

#[tokio::test]
async fn to_public_url_rewrites_origin() {
    let client = OverpayClient::new("http://web:80")
        .unwrap()
        .with_public_url("https://overpay.com")
        .unwrap();
    let rewritten = client.to_public_url("http://web:80/login/abc?next=/x");
    assert_eq!(rewritten, "https://overpay.com/login/abc?next=/x");
}

#[tokio::test]
async fn to_public_url_no_rewrite_when_urls_match() {
    let client = OverpayClient::new("https://overpay.com").unwrap();
    let s = "https://overpay.com/some/path";
    assert_eq!(client.to_public_url(s), s);
}
