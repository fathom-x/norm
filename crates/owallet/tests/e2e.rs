//! End-to-end tests: real Rails marketplace + live `owallet serve` driven
//! over `/mcp`, plus a real bot_manager seller poll for fulfillment.
//!
//! Port of the deleted Python pytest E2E suite
//! (`owallet/wallet_mcp/tests/test_e2e_*.py`). Each scenario is `#[ignore]`d
//! because it needs a prepared Postgres test DB for both the main app and
//! bot_manager; the dedicated `owallet-rs-e2e` workflow runs them with
//! `cargo test -p owallet --test e2e -- --ignored`.

mod common;

use common::{fulfill_as_seller, OwalletServer, RailsServer};
use serde_json::json;

/// E2E triangle: pre-loaded credits → bot fulfills → owallet sees delivered.
///
/// The full owallet ↔ marketplace ↔ bot_manager loop, off-chain. owallet is
/// authenticated as the seeded buyer via a stored Rails bearer, so its
/// `get_wallet_orders` tool sees the credit-funded order as delivered.
#[test]
#[ignore = "needs Rails + Postgres + bot_manager (run in owallet-rs-e2e workflow)"]
fn triangle_credit_funded_order_is_fulfilled_and_visible() {
    let rails = RailsServer::start();

    // 1. Seller (+ listing) and buyer.
    let seller = rails.seed(
        "seed_seller",
        json!({
            "seller_slug": "credit-shop-rust",
            "with_usdc_wallet": true,
            "listing_title": "Hello",
            "price_cents": 1000,
            "delivered_content_type": "text/plain",
        }),
    );
    let buyer = rails.seed("seed_buyer", json!({}));
    let buyer_token = buyer["api_token"].as_str().unwrap();

    // 2. Pre-load credits and create an order paid entirely by them.
    rails.seed(
        "seed_credits",
        json!({
            "buyer_id": buyer["user_id"],
            "seller_id": seller["user_id"],
            "balance_cents": 5000,
        }),
    );
    let order = rails.seed(
        "seed_credit_order",
        json!({ "buyer_id": buyer["user_id"], "listing_id": seller["listing_id"] }),
    );
    assert_eq!(
        order["payment_status"], "paid",
        "credits did not fully pay order: {order}"
    );
    let order_id = order["order_id"].as_str().unwrap().to_string();

    // 3. Route paid → awaiting_seller, then a real bot delivers it.
    rails.run_job("FulfillPaidOrdersJob");
    let delivered = fulfill_as_seller(
        &rails.base_url,
        seller["api_token"].as_str().unwrap(),
        "World",
        None,
    );
    assert!(
        delivered.contains(&order_id),
        "bot did not deliver order: {delivered:?}"
    );

    // 4. owallet (as the buyer) sees the order delivered + paid.
    let owallet = OwalletServer::start(&rails.base_url, Some(buyer_token));
    let payload = owallet.call_tool("get_wallet_orders", json!({}));
    let seen = payload["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["id"].as_str() == Some(order_id.as_str()))
        .unwrap_or_else(|| panic!("owallet did not see the order: {payload}"));
    assert_eq!(seen["fulfillment_status"], "delivered");
    assert_eq!(seen["payment_status"], "paid");
}

/// E2E: owallet lists its orders over NIP-98 against a live marketplace.
///
/// Seeds a paid settlement order whose payer_address is owallet's address,
/// then calls `get_wallet_orders` with no stored bearer — owallet signs a
/// NIP-98 event the Rails verifier must accept. Exercises the cross-language
/// NIP-98 wire format + Nostr→EVM address derivation over real HTTP.
#[test]
#[ignore = "needs Rails + Postgres (run in owallet-rs-e2e workflow)"]
fn orders_nip98_sees_seeded_order() {
    let rails = RailsServer::start();
    // No Rails bearer → owallet authenticates via NIP-98 for its own address.
    let owallet = OwalletServer::start(&rails.base_url, None);
    let payer = owallet.address.clone();

    let seller = rails.seed(
        "seed_seller",
        json!({
            "seller_slug": format!("seller-rust-{}", &payer[2..10]),
            "with_usdc_wallet": true,
            "listing_title": "Digital Good",
            "price_cents": 1000,
            "delivered_content_type": "text/plain",
        }),
    );
    let seeded = rails.seed(
        "seed_settlement_order",
        json!({
            "listing_id": seller["listing_id"],
            "payer_address": payer,
            "deliver": true,
            "delivered_content": "THANK-YOU-9999",
        }),
    );
    let order_id = seeded["order_id"].as_str().unwrap();

    let payload = owallet.call_tool("get_wallet_orders", json!({}));
    let order = payload["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["id"].as_str() == Some(order_id))
        .unwrap_or_else(|| panic!("seeded order not visible: {payload}"));
    assert_eq!(order["payment_status"], "paid");
    assert_eq!(order["fulfillment_status"], "delivered");
    assert_eq!(order["product_title"], "Digital Good");
    assert_eq!(order["listing"]["id"], seller["listing_id"]);
}

/// E2E: full MCP-driven buy flow against a schema-bearing listing.
///
/// Buyer (owallet, bearer-authenticated) drives the whole flow over MCP —
/// `create_order` → `redeem_merchant_credits` → bot fulfills →
/// `get_order_status` confirms delivered. The listing carries a
/// `buyer_note_schema` mandating an object with a `code` field; the bot
/// JSON-parses buyer_note and echoes the `code` value back, so a regression
/// that sent a free-text buyer_note (the live bug) would fail here.
#[test]
#[ignore = "needs Rails + Postgres + bot_manager (run in owallet-rs-e2e workflow)"]
fn mcp_buy_flow_delivers_schema_buyer_note() {
    let rails = RailsServer::start();

    let seller = rails.seed(
        "seed_seller",
        json!({
            "seller_slug": "code-exec-rust",
            "with_usdc_wallet": true,
            "listing_title": "Run Python Code",
            "price_cents": 5,
            "delivered_content_type": "text/plain",
            "buyer_note_schema": {
                "type": "object",
                "properties": { "code": { "type": "string" } },
                "required": ["code"],
            },
        }),
    );
    let buyer = rails.seed("seed_buyer", json!({}));
    let buyer_token = buyer["api_token"].as_str().unwrap();
    rails.seed(
        "seed_credits",
        json!({
            "buyer_id": buyer["user_id"],
            "seller_id": seller["user_id"],
            "balance_cents": 100,
        }),
    );

    // owallet acts as the buyer for every Rails write below.
    let owallet = OwalletServer::start(&rails.base_url, Some(buyer_token));

    let code = "print(\"hello from overpay\")\nprint(2 + 2)";
    let created = owallet.call_tool(
        "create_order",
        json!({
            "listing_id": seller["listing_id"],
            "buyer_note": json!({ "code": code }).to_string(),
        }),
    );
    let created = &created["data"];
    let order_id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["payment_status"], "pending");

    let redeemed = owallet.call_tool(
        "redeem_merchant_credits",
        json!({ "seller_slug": seller["seller_slug"], "order_id": order_id }),
    );
    assert_eq!(redeemed["data"]["status"], "fully_paid", "{redeemed}");

    // Route paid → awaiting_seller, then the bot delivers by echoing `code`.
    rails.run_job("FulfillPaidOrdersJob");
    let delivered = fulfill_as_seller(
        &rails.base_url,
        seller["api_token"].as_str().unwrap(),
        "World",
        Some("code"),
    );
    assert!(
        delivered.contains(&order_id),
        "bot did not deliver: {delivered:?}"
    );

    let status = owallet.call_tool("get_order_status", json!({ "order_id": order_id }));
    let status = &status["data"];
    assert_eq!(status["payment_status"], "paid");
    assert_eq!(status["fulfillment_status"], "delivered");
}
