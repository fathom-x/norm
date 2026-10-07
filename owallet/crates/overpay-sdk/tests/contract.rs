//! The SDK against the API contract: the spec copy in `spec/` and the
//! fixtures the marketplace's own tests pin (both synced by Overpay's
//! `bin/sync-openapi`).

use std::collections::BTreeSet;

use overpay_sdk::models::{
    Account, BuyerRegistration, CreatedOrder, CreditLoad, CreditPurchase, CreditTransaction,
    Listing, MerchantCredit, MerchantCreditBalance, OAuthClient, OAuthServerMetadata, OAuthToken,
    Order, Page, Redemption, Seller, SpendAuthorization, WebSession,
};
use overpay_sdk::{ApiError, Audience, ErrorCode, FromBody, OPERATIONS};
use serde::Serialize;
use serde_json::Value;

const SPEC: &str = include_str!("../spec/overpay-v1.json");
const INDEX: &str = include_str!("../spec/fixtures/index.json");
const GENERATED: &str = include_str!("../src/generated.rs");

/// Buyer operations this SDK implements, and the method that does.
const IMPLEMENTED: &[(&str, &str)] = &[
    ("listListings", "listings().list"),
    ("getListing", "listings().get"),
    ("getSeller", "sellers().get"),
    ("listOrders", "orders().list"),
    ("createOrder", "orders().create"),
    ("getOrder", "orders().get"),
    ("createSpendAuthorization", "spend_authorizations().create"),
    ("getSpendAuthorization", "spend_authorizations().get"),
    ("closeSpendAuthorization", "spend_authorizations().close"),
    ("listMerchantCredits", "credits().list"),
    ("listMerchantCreditTransactions", "credits().transactions"),
    ("loadCoreCredits", "credits().load"),
    ("getMerchantCreditBalance", "credits().get"),
    ("purchaseMerchantCredits", "credits().purchase"),
    ("redeemMerchantCredits", "credits().redeem"),
    ("getAccount", "account().get"),
    ("registerBuyer", "buyer().register"),
    ("createWebSession", "buyer().web_session"),
    ("getOAuthMetadata", "oauth().metadata"),
    ("registerOAuthClient", "oauth().register_client"),
    ("authorizeOAuth", "oauth().authorize_url (browser)"),
    ("approveOAuth", "the user's browser"),
    ("exchangeOAuthToken", "oauth().exchange_code"),
];

fn spec() -> Value {
    serde_json::from_str(SPEC).unwrap()
}

#[test]
fn generated_is_fresh() {
    assert!(
        GENERATED == overpay_sdk_codegen::render(&spec()),
        "src/generated.rs is stale: run `cargo run -p overpay-sdk-codegen`"
    );
}

#[test]
fn every_buyer_operation_is_implemented() {
    let buyer: BTreeSet<&str> = OPERATIONS
        .iter()
        .filter(|op| op.audience == Audience::Buyer)
        .map(|op| op.id)
        .collect();
    let implemented: BTreeSet<&str> = IMPLEMENTED.iter().map(|(id, _)| *id).collect();
    assert_eq!(
        buyer, implemented,
        "buyer operations in the spec vs. IMPLEMENTED"
    );
}

#[test]
fn the_operation_table_matches_the_spec() {
    let spec = spec();
    for op in OPERATIONS {
        let found = &spec["paths"][op.path][op.method.to_lowercase()];
        assert_eq!(found["operationId"], op.id, "{} {}", op.method, op.path);
    }
}

/// Parse a fixture with its operation's model, serialize it back, and
/// compare: a typed model must keep every field the API sent.
fn roundtrip<T: FromBody + Serialize>(name: &str, body: &Value, part: impl Fn(&Value) -> &Value) {
    let parsed = T::from_body(body).unwrap_or_else(|e| panic!("{name}: {e}"));
    let back = serde_json::to_value(&parsed).unwrap();
    assert_eq!(
        &back,
        part(body),
        "{name} doesn't survive a typed round trip"
    );
}

fn data(body: &Value) -> &Value {
    &body["data"]
}

fn whole(body: &Value) -> &Value {
    body
}

#[test]
fn every_fixture_round_trips_through_its_model() {
    let index: serde_json::Map<String, Value> = serde_json::from_str(INDEX).unwrap();
    assert_eq!(
        index.len(),
        overpay_sdk::testing::FIXTURES.len(),
        "testing::FIXTURES lists every fixture"
    );
    for (file, entry) in &index {
        let stem = file.trim_end_matches(".json");
        let body = overpay_sdk::testing::fixture(stem);
        let status = entry["status"].as_u64().unwrap();
        if status >= 400 {
            let error = ApiError::from_response(status as u16, body.to_string().as_bytes());
            assert!(
                matches!(error.code, Some(ref c) if c.is_known()),
                "{file}: error without a known code"
            );
            continue;
        }
        match entry["operation"].as_str().unwrap() {
            "listListings" => roundtrip::<Page<Listing>>(file, &body, whole),
            "getListing" => roundtrip::<Listing>(file, &body, data),
            "getSeller" => roundtrip::<Seller>(file, &body, data),
            "listOrders" => roundtrip::<Page<Order>>(file, &body, whole),
            "getOrder" => roundtrip::<Order>(file, &body, data),
            "createOrder" => roundtrip::<CreatedOrder>(file, &body, whole),
            "createSpendAuthorization" | "getSpendAuthorization" | "closeSpendAuthorization" => {
                roundtrip::<SpendAuthorization>(file, &body, data);
            }
            "listMerchantCredits" => roundtrip::<Vec<MerchantCredit>>(file, &body, data),
            "listMerchantCreditTransactions" => {
                roundtrip::<Page<CreditTransaction>>(file, &body, whole)
            }
            "loadCoreCredits" => roundtrip::<CreditLoad>(file, &body, data),
            "getMerchantCreditBalance" => roundtrip::<MerchantCreditBalance>(file, &body, data),
            "purchaseMerchantCredits" => roundtrip::<CreditPurchase>(file, &body, data),
            "redeemMerchantCredits" => roundtrip::<Redemption>(file, &body, data),
            "getAccount" => roundtrip::<Account>(file, &body, data),
            "registerBuyer" => roundtrip::<BuyerRegistration>(file, &body, data),
            "createWebSession" => roundtrip::<WebSession>(file, &body, data),
            "getOAuthMetadata" => roundtrip::<OAuthServerMetadata>(file, &body, whole),
            "registerOAuthClient" => roundtrip::<OAuthClient>(file, &body, whole),
            "exchangeOAuthToken" => roundtrip::<OAuthToken>(file, &body, whole),
            other => panic!("{file}: no model mapped for {other}"),
        }
    }
}

#[test]
fn typed_views_read_the_interesting_parts() {
    use overpay_sdk::testing::fixture;
    let listing = Listing::from_body(&fixture("listings_show_variants")).unwrap();
    assert!(listing.is_metered());
    assert_eq!(listing.provider_tool.as_ref().unwrap().name, "infer");
    assert_eq!(
        listing.authorization_bounds(Some("premium")),
        (Some(100), Some(2000))
    );
    assert_eq!(listing.clamp_authorization(Some("cheap"), 9_999), 500);
    assert!(listing.buyer_note_schema().is_some());

    let rejected = Order::from_body(&fixture("orders_show_rejected")).unwrap();
    assert!(rejected.is_terminal());
    let rejection = rejected.rejection.unwrap();
    assert_eq!(
        rejection.reason_code,
        overpay_sdk::RejectionReason::AuthorizationTooLow
    );
    assert_eq!(rejection.required_authorization_cents.unwrap().ceil(), 150);

    let offloaded = Order::from_body(&fixture("orders_show_offloaded")).unwrap();
    assert!(offloaded.delivered_content_url.is_some() && offloaded.delivered_content().is_none());

    let partial = Order::from_body(&fixture("orders_show_partial")).unwrap();
    assert_eq!(partial.partial_seq, Some(1));

    let paid = CreatedOrder::from_body(&fixture("orders_create_paid")).unwrap();
    assert!(paid.payment.unwrap().is_settled());

    let credits = Vec::<MerchantCredit>::from_body(&fixture("merchant_credits_index")).unwrap();
    assert_eq!(credits.len(), 2);

    let insufficient = ApiError::from_response(
        402,
        fixture("error_insufficient_credits").to_string().as_bytes(),
    );
    assert_eq!(insufficient.code, Some(ErrorCode::InsufficientCredits));
    assert!(insufficient.is_insufficient_credits());
    assert!(insufficient.detail_f64("authorization_cents").is_some());
}

#[test]
fn unknown_enum_values_survive() {
    let status: overpay_sdk::FulfillmentStatus =
        serde_json::from_value(serde_json::json!("teleported")).unwrap();
    assert_eq!(
        status,
        overpay_sdk::FulfillmentStatus::Other("teleported".into())
    );
    assert!(!status.is_known());
    assert_eq!(serde_json::to_value(&status).unwrap(), "teleported");
}
