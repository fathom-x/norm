//! A fake marketplace for tests of code built on this SDK (feature
//! `testing`): a wiremock server, the contract fixtures the real API is
//! pinned to, and helpers to mount them.

use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::{Client, RetryPolicy};

macro_rules! fixtures {
    ($($name:literal),* $(,)?) => {
        /// Every contract fixture: (file stem, JSON text).
        pub const FIXTURES: &[(&str, &str)] = &[
            $(($name, include_str!(concat!("../spec/fixtures/", $name, ".json"))),)*
        ];
    };
}

fixtures!(
    "account_show",
    "buyer_register",
    "error_insufficient_credits",
    "error_not_found",
    "error_unauthorized",
    "error_unknown_variant",
    "listings_index",
    "listings_show_variants",
    "merchant_credit_purchase",
    "merchant_credit_transactions",
    "merchant_credits_index",
    "merchant_credits_load",
    "merchant_credits_redeem",
    "merchant_credits_show",
    "oauth_client",
    "oauth_metadata",
    "oauth_token",
    "orders_create_paid",
    "orders_create_unpaid",
    "orders_index",
    "orders_show_delivered",
    "orders_show_offloaded",
    "orders_show_partial",
    "orders_show_rejected",
    "sellers_show",
    "spend_authorization_close",
    "spend_authorization_create",
    "spend_authorization_show",
    "web_session",
);

/// A contract fixture by file stem (e.g. `"orders_show_delivered"`): a real
/// response body the marketplace's tests pin.
///
/// # Panics
/// On an unknown name.
#[must_use]
pub fn fixture(name: &str) -> Value {
    let (_, text) = FIXTURES
        .iter()
        .find(|(n, _)| *n == name)
        .unwrap_or_else(|| panic!("no fixture {name}"));
    serde_json::from_str(text).expect("fixtures are JSON")
}

/// A fake Overpay marketplace.
pub struct FakeOverpay {
    server: MockServer,
}

impl FakeOverpay {
    pub async fn start() -> Self {
        Self {
            server: MockServer::start().await,
        }
    }

    #[must_use]
    pub fn uri(&self) -> String {
        self.server.uri()
    }

    #[must_use]
    pub fn server(&self) -> &MockServer {
        &self.server
    }

    /// A client for this fake, without retries.
    ///
    /// # Panics
    /// Never in practice (the fake's URI is valid).
    #[must_use]
    pub fn client(&self) -> Client {
        Client::builder(self.uri())
            .retry(RetryPolicy::none())
            .build()
            .expect("client")
    }

    /// Answer `verb path` with `status` and `body`.
    pub async fn mount(&self, verb: &str, route: &str, status: u16, body: Value) {
        Mock::given(method(verb))
            .and(path(route))
            .respond_with(ResponseTemplate::new(status).set_body_json(body))
            .mount(&self.server)
            .await;
    }

    /// Answer `verb path` with an API error.
    pub async fn mount_error(
        &self,
        verb: &str,
        route: &str,
        status: u16,
        code: &str,
        message: &str,
    ) {
        self.mount(
            verb,
            route,
            status,
            json!({ "error": message, "code": code }),
        )
        .await;
    }

    /// Serve `order` (a bare order object) at `GET /api/v1/orders/{id}`.
    pub async fn mount_order(&self, order: Value) {
        let id = order["id"].as_str().expect("order id").to_string();
        self.mount(
            "GET",
            &format!("/api/v1/orders/{id}"),
            200,
            json!({ "data": order }),
        )
        .await;
    }

    /// Serve `listings` (bare listing objects) as one page of the index,
    /// and each at its own detail route.
    pub async fn mount_listings(&self, listings: Vec<Value>) {
        for listing in &listings {
            let id = listing["id"].as_str().expect("listing id");
            self.mount(
                "GET",
                &format!("/api/v1/listings/{id}"),
                200,
                json!({ "data": listing }),
            )
            .await;
        }
        self.mount(
            "GET",
            "/api/v1/listings",
            200,
            json!({ "data": listings, "next_cursor": null }),
        )
        .await;
    }
}

/// The delivered-order fixture's order, with `fields` merged over it.
#[must_use]
pub fn order_with(fields: Value) -> Value {
    let mut order = fixture("orders_show_delivered")["data"].clone();
    if let (Some(base), Some(over)) = (order.as_object_mut(), fields.as_object()) {
        for (k, v) in over {
            base.insert(k.clone(), v.clone());
        }
    }
    order
}

/// The metered-listing fixture, with `fields` merged over it.
#[must_use]
pub fn listing_with(fields: Value) -> Value {
    let mut listing = fixture("listings_show_variants")["data"].clone();
    if let (Some(base), Some(over)) = (listing.as_object_mut(), fields.as_object()) {
        for (k, v) in over {
            base.insert(k.clone(), v.clone());
        }
    }
    listing
}
