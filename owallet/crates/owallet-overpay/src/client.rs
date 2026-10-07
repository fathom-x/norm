//! Async REST client for the Overpay Rails API — owallet's compatibility
//! layer over [`overpay_sdk`].
//!
//! Every call is the SDK's resource call (auth headers, NIP-98 signing,
//! error parsing, safe retries); this type only keeps owallet's
//! established call shapes — the raw-`Value` passthroughs and the lenient
//! compat models. New code should use the SDK directly
//! ([`OverpayClient::sdk`]).

use overpay_sdk::{
    CreateOrder, FulfillmentStatus, ListingQuery, OrderQuery, PaymentStatus, Response,
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use url::Url;

use crate::error::OverpayError;
use crate::models::{
    AccountInfo, LightningLoadResponse, ListingFilters, MerchantCreditsList, OAuthTokenResponse,
    Order, OrderFilters, PurchaseCreditsResponse, WebSessionResponse,
};

pub use overpay_sdk::host_key;

use overpay_sdk::Auth;

#[derive(Clone)]
pub struct OverpayClient {
    sdk: overpay_sdk::Client,
}

impl OverpayClient {
    pub fn new(base_url: &str) -> Result<Self, OverpayError> {
        Self::with_urls(base_url, None)
    }

    /// A client for the API at `base_url`. Browser-facing URLs (the OAuth
    /// authorize page, [`Self::to_public_url`]) use `public_url` when it is
    /// given — Docker / reverse-proxy setups where browsers reach the
    /// marketplace on another host.
    pub fn with_urls(base_url: &str, public_url: Option<&str>) -> Result<Self, OverpayError> {
        let mut builder = overpay_sdk::Client::builder(base_url)
            .user_agent(concat!("owallet/", env!("CARGO_PKG_VERSION")));
        if let Some(public_url) = public_url {
            builder = builder.public_url(public_url);
        }
        Ok(Self {
            sdk: builder.build()?,
        })
    }

    /// The SDK client this wraps.
    #[must_use]
    pub fn sdk(&self) -> &overpay_sdk::Client {
        &self.sdk
    }

    /// Rewrite a Rails-generated URL to use the public-facing host.
    #[must_use]
    pub fn to_public_url(&self, raw: &str) -> String {
        self.sdk.to_public_url(raw)
    }

    pub fn base_url(&self) -> &Url {
        self.sdk.base_url()
    }

    /// Token-store key for the Overpay host this client talks to. Deriving
    /// it from the client means a caller can't file a bearer under one host
    /// and look it up under another. See [`host_key`].
    #[must_use]
    pub fn host_key(&self) -> String {
        self.sdk.host_key()
    }

    // ---- OAuth (PKCE) ----

    /// Exchange a PKCE authorization code for an access token.
    pub async fn exchange_code(
        &self,
        client_id: &str,
        code: &str,
        verifier: &str,
        redirect_uri: &str,
    ) -> Result<OAuthTokenResponse, OverpayError> {
        decode(
            self.sdk
                .oauth()
                .exchange_code(client_id, code, verifier, redirect_uri)
                .await?,
        )
    }

    // ---- API endpoints ----
    //
    // The `_value` variants return the Rails response verbatim — envelope
    // included, nothing flattened — so MCP consumers see byte-identical
    // output (fathom-x/overpay#288).

    pub async fn account(&self, auth: Auth<'_>) -> Result<AccountInfo, OverpayError> {
        decode(self.sdk.account().get(auth).await?)
    }

    /// Raw-`Value` variant of [`Self::account`], for `get_account_info`.
    pub async fn account_value(&self, auth: Auth<'_>) -> Result<Value, OverpayError> {
        Ok(self.sdk.account().get(auth).await?.into_raw())
    }

    pub async fn web_session(&self, auth: Auth<'_>) -> Result<WebSessionResponse, OverpayError> {
        decode(self.sdk.buyer().web_session(auth).await?)
    }

    /// One page of the listing index (public, no auth).
    pub async fn list_listings_value(
        &self,
        filters: &ListingFilters,
    ) -> Result<Value, OverpayError> {
        let query = listing_query(filters);
        Ok(self.sdk.listings().list(&query).await?.into_raw())
    }

    /// One page of the buyer's orders.
    pub async fn list_orders_value(
        &self,
        auth: Auth<'_>,
        filters: &OrderFilters,
    ) -> Result<Value, OverpayError> {
        let query = order_query(filters);
        Ok(self.sdk.orders().list(&query, auth).await?.into_raw())
    }

    pub async fn get_order(&self, id: &str, auth: Auth<'_>) -> Result<Order, OverpayError> {
        decode(self.sdk.orders().get(id, auth).await?)
    }

    /// Raw-`Value` variant of [`Self::get_order`].
    pub async fn get_order_value(&self, id: &str, auth: Auth<'_>) -> Result<Value, OverpayError> {
        Ok(self.sdk.orders().get(id, auth).await?.into_raw())
    }

    /// Place an order, returning the raw response. `buyer_note` is sent as
    /// a JSON string (the seller bot `JSON.parse`s it): serialize a
    /// structured note to a string first. `client_session_id` labels the
    /// order with the session it belongs to (see
    /// [`OrderFilters::client_session_id`]); with `None` the key is left
    /// out of the body.
    ///
    /// The SDK keys the create with an automatic `Idempotency-Key`, so it is
    /// retried where the marketplace certainly didn't act (connection
    /// refused, 429, 503). It is not retried after a timeout, 502 or 504 —
    /// the order may have been placed — because owallet leaves the SDK's
    /// `retry_ambiguous_writes` off.
    pub async fn create_order_value(
        &self,
        listing_id: &str,
        buyer_note: Option<&str>,
        client_session_id: Option<&str>,
        auth: Auth<'_>,
    ) -> Result<Value, OverpayError> {
        let mut order = CreateOrder::new(listing_id);
        order.buyer_note = buyer_note.map(str::to_string);
        order.client_session_id = client_session_id.map(str::to_string);
        Ok(self.sdk.orders().create(&order, auth).await?.into_raw())
    }

    /// One listing in full (`GET /api/v1/listings/{id}`), raw: the
    /// `{data: {...}}` envelope with `buyer_note_schema` /
    /// `checkout_schema`. Public, no auth.
    pub async fn get_listing_value(&self, id: &str) -> Result<Value, OverpayError> {
        Ok(self.sdk.listings().get(id).await?.into_raw())
    }

    // ---- Merchant credits ----

    pub async fn list_merchant_credits(
        &self,
        auth: Auth<'_>,
    ) -> Result<MerchantCreditsList, OverpayError> {
        decode(self.sdk.credits().list(auth).await?)
    }

    /// Raw-`Value` variant of [`Self::list_merchant_credits`].
    pub async fn list_merchant_credits_value(&self, auth: Auth<'_>) -> Result<Value, OverpayError> {
        Ok(self.sdk.credits().list(auth).await?.into_raw())
    }

    pub async fn purchase_merchant_credits(
        &self,
        seller_slug: &str,
        amount_cents: i64,
        auth: Auth<'_>,
    ) -> Result<PurchaseCreditsResponse, OverpayError> {
        decode(
            self.sdk
                .credits()
                .purchase(seller_slug, amount_cents, auth)
                .await?,
        )
    }

    /// Load core marketplace credits via Lightning
    /// (`POST /api/v1/merchant_credits/load`): a BOLT11 invoice plus the
    /// order to poll.
    pub async fn load_core_credits(
        &self,
        amount_cents: i64,
        auth: Auth<'_>,
    ) -> Result<LightningLoadResponse, OverpayError> {
        decode(self.sdk.credits().load(amount_cents, auth).await?)
    }

    /// Pay a pending order with credits held at `seller_slug`, returning
    /// the raw response.
    pub async fn redeem_merchant_credits_value(
        &self,
        seller_slug: &str,
        order_id: &str,
        auth: Auth<'_>,
    ) -> Result<Value, OverpayError> {
        Ok(self
            .sdk
            .credits()
            .redeem(seller_slug, order_id, auth)
            .await?
            .into_raw())
    }
}

/// An SDK response read into one of owallet's lenient compat models.
fn decode<T: DeserializeOwned, U>(response: Response<U>) -> Result<T, OverpayError> {
    Ok(serde_json::from_value(response.into_raw())?)
}

/// `seller_slug` is sent as Rails's `seller` parameter.
fn listing_query(filters: &ListingFilters) -> ListingQuery {
    let mut query = ListingQuery::default();
    query.category.clone_from(&filters.category);
    query.seller.clone_from(&filters.seller_slug);
    query.cursor.clone_from(&filters.cursor);
    query.limit = filters.limit;
    query
}

fn order_query(filters: &OrderFilters) -> OrderQuery {
    let mut query = OrderQuery::default();
    query.payment_status = filters.payment_status.as_deref().map(PaymentStatus::from);
    query.fulfillment_status = filters
        .fulfillment_status
        .as_deref()
        .map(FulfillmentStatus::from);
    query.cursor.clone_from(&filters.cursor);
    query.limit = filters.limit;
    query.payer_address.clone_from(&filters.payer_address);
    query
        .client_session_id
        .clone_from(&filters.client_session_id);
    query
}

#[cfg(test)]
mod host_key_tests {
    use super::*;

    #[test]
    fn trailing_slash_does_not_change_the_key() {
        assert_eq!(host_key("http://localhost:3001/"), "http://localhost:3001");
        assert_eq!(host_key("http://localhost:3001"), "http://localhost:3001");
    }

    #[test]
    fn client_and_free_fn_agree() {
        // The CLI has the raw configured URL; `serve` only has the built
        // client. Both have to land on the same row.
        let raw = "http://localhost:3001/";
        let client = OverpayClient::new(raw).unwrap();
        assert_eq!(client.host_key(), host_key(raw));
    }

    #[test]
    fn a_path_prefixed_host_keeps_its_path() {
        assert_eq!(
            host_key("https://gw.example/overpay/"),
            "https://gw.example/overpay"
        );
    }

    #[test]
    fn an_unparseable_url_falls_back_to_a_trim() {
        assert_eq!(host_key("not a url/"), "not a url");
    }
}
