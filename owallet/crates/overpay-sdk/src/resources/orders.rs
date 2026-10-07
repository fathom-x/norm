use serde_json::{json, Map, Value};

use crate::auth::Auth;
use crate::client::{Client, Request};
use crate::error::Error;
use crate::generated::{FulfillmentStatus, PaymentStatus};
use crate::models::{CreatedOrder, Order, Page};
use crate::response::Response;

use super::segment;

/// Filters for [`OrdersApi::list`].
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrderQuery {
    pub payment_status: Option<PaymentStatus>,
    pub fulfillment_status: Option<FulfillmentStatus>,
    pub cursor: Option<String>,
    /// Page size (the API caps it at 20).
    pub limit: Option<u32>,
    /// The EVM payer address — required with NIP-98 auth, which can only
    /// read the x402 orders of an address derived from the signing key.
    pub payer_address: Option<String>,
    /// Exactly these orders, in detail form.
    pub ids: Vec<String>,
    /// Only orders created with this [`CreateOrder::client_session_id`]
    /// (exact match; an unknown label is an empty page). A marketplace that
    /// predates the field ignores the filter and returns every order, whose
    /// rows then carry no `client_session_id`: check for it.
    pub client_session_id: Option<String>,
}

/// Options for [`OrdersApi::get_with`].
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GetOrder {
    /// The `partial_seq` you already hold: an unchanged streaming buffer
    /// is then left out of the response.
    pub since_seq: Option<u64>,
    /// See [`OrderQuery::payer_address`].
    pub payer_address: Option<String>,
}

/// A new order. Build with [`CreateOrder::new`].
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CreateOrder {
    pub listing_id: String,
    /// Free text, or a JSON document as a string (see the listing's
    /// `buyer_note_schema`).
    pub buyer_note: Option<String>,
    /// An active variant key.
    pub variant: Option<String>,
    /// Pay with merchant credits in the same request.
    pub pay_with_credits: bool,
    /// Metered listings: the most the seller may charge. Requires paying
    /// with credits.
    pub authorization_cents: Option<i64>,
    /// Size and pay this order from a spend authorization. Requires paying
    /// with credits.
    pub spend_authorization_id: Option<String>,
    /// The buyer's own opaque label for the session (conversation, run,
    /// job) this order belongs to, at most 256 characters; list a session's
    /// orders with [`OrderQuery::client_session_id`]. Never shown to the
    /// seller, and separate from the buyer note (the seller's input).
    pub client_session_id: Option<String>,
    /// Makes retries safe: a repeat with the same key replays the first
    /// response. When `None` and the client's automatic keys are on, one is
    /// generated per call (and reused across that call's retries).
    pub idempotency_key: Option<String>,
}

impl CreateOrder {
    #[must_use]
    pub fn new(listing_id: impl Into<String>) -> Self {
        Self {
            listing_id: listing_id.into(),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn buyer_note(mut self, note: impl Into<String>) -> Self {
        self.buyer_note = Some(note.into());
        self
    }

    /// A structured buyer_note, sent as its JSON text.
    #[must_use]
    pub fn buyer_note_json(mut self, note: &Value) -> Self {
        self.buyer_note = Some(match note {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        });
        self
    }

    #[must_use]
    pub fn variant(mut self, key: impl Into<String>) -> Self {
        self.variant = Some(key.into());
        self
    }

    #[must_use]
    pub fn pay_with_credits(mut self) -> Self {
        self.pay_with_credits = true;
        self
    }

    /// Authorize `cents` (and pay with credits, which it requires).
    #[must_use]
    pub fn authorization_cents(mut self, cents: i64) -> Self {
        self.authorization_cents = Some(cents);
        self.pay_with_credits = true;
        self
    }

    /// Order under a spend authorization (and pay with credits).
    #[must_use]
    pub fn spend_authorization(mut self, id: impl Into<String>) -> Self {
        self.spend_authorization_id = Some(id.into());
        self.pay_with_credits = true;
        self
    }

    /// Label the order with the session it belongs to (see
    /// [`Self::client_session_id`]).
    #[must_use]
    pub fn client_session_id(mut self, label: impl Into<String>) -> Self {
        self.client_session_id = Some(label.into());
        self
    }

    #[must_use]
    pub fn idempotency_key(mut self, key: impl Into<String>) -> Self {
        self.idempotency_key = Some(key.into());
        self
    }

    /// The request body (`listing_id` plus only the fields that are set).
    #[must_use]
    pub fn body(&self) -> Value {
        let mut body = Map::new();
        body.insert("listing_id".into(), json!(self.listing_id));
        if let Some(note) = &self.buyer_note {
            body.insert("buyer_note".into(), json!(note));
        }
        if let Some(variant) = &self.variant {
            body.insert("variant".into(), json!(variant));
        }
        if self.pay_with_credits {
            body.insert("pay".into(), json!("merchant_credits"));
        }
        if let Some(cents) = self.authorization_cents {
            body.insert("authorization_cents".into(), json!(cents));
        }
        if let Some(id) = &self.spend_authorization_id {
            body.insert("spend_authorization_id".into(), json!(id));
        }
        if let Some(label) = &self.client_session_id {
            body.insert("client_session_id".into(), json!(label));
        }
        Value::Object(body)
    }
}

/// Orders: list, read, place.
#[derive(Debug, Clone, Copy)]
pub struct OrdersApi<'a>(pub(crate) &'a Client);

impl OrdersApi<'_> {
    /// One page of the caller's orders, newest first.
    pub async fn list(
        &self,
        query: &OrderQuery,
        auth: Auth<'_>,
    ) -> Result<Response<Page<Order>>, Error> {
        let mut request = Request::get("/api/v1/orders")
            .query("payment_status", query.payment_status.as_ref())
            .query("fulfillment_status", query.fulfillment_status.as_ref())
            .query("cursor", query.cursor.as_deref())
            .query("limit", query.limit)
            .query("payer_address", query.payer_address.as_deref())
            .query("client_session_id", query.client_session_id.as_deref());
        for id in &query.ids {
            request = request.query("ids[]", Some(id));
        }
        Ok(Response::new(self.0.execute(request, auth).await?))
    }

    /// One order in detail.
    pub async fn get(&self, id: &str, auth: Auth<'_>) -> Result<Response<Order>, Error> {
        self.get_with(id, &GetOrder::default(), auth).await
    }

    pub async fn get_with(
        &self,
        id: &str,
        options: &GetOrder,
        auth: Auth<'_>,
    ) -> Result<Response<Order>, Error> {
        let request = Request::get(format!("/api/v1/orders/{}", segment(id)))
            .query("since_seq", options.since_seq)
            .query("payer_address", options.payer_address.as_deref());
        Ok(Response::new(self.0.execute(request, auth).await?))
    }

    /// Place an order (Bearer auth only). With [`CreateOrder::pay_with_credits`]
    /// the response's `payment` says how paying went; a redemption failure
    /// still returns the (pending) order.
    pub async fn create(
        &self,
        order: &CreateOrder,
        auth: Auth<'_>,
    ) -> Result<Response<CreatedOrder>, Error> {
        let key = order
            .idempotency_key
            .clone()
            .or_else(|| self.0.auto_idempotency_key());
        let request = Request::post("/api/v1/orders")
            .json(order.body())
            .idempotency_key(key);
        Ok(Response::new(self.0.execute(request, auth).await?))
    }
}
