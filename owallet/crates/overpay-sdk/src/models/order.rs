use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::common::Cents;
use crate::generated::{
    FulfillmentStatus, PaymentOutcomeStatus, PaymentStatus, RefundStatus, RejectionReason,
};

/// An order. List responses carry the summary fields; `orders().get` (and
/// a list by `ids`) adds the detail fields: the deliverable, inline or
/// offloaded, and a streaming seller's output so far.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Order {
    pub id: String,
    pub payment_status: PaymentStatus,
    pub fulfillment_status: FulfillmentStatus,
    pub product_title: String,
    pub free: bool,
    pub product_price_cents: Cents,
    pub product_price_usd: String,
    pub total_usd_cents: Cents,
    pub total_usd: String,
    /// What the buyer actually paid after refunds.
    pub settled_amount_cents: Cents,
    pub refunded_cents: Cents,
    pub refund_status: RefundStatus,
    pub fulfillment_error: Option<String>,
    /// What the buyer committed when the order was paid.
    pub authorized_cents: Option<Cents>,
    /// What the seller finally kept; `None` until it captures or rejects.
    pub captured_cents: Option<Cents>,
    pub released_cents: Option<Cents>,
    pub rejection: Option<Rejection>,
    pub spend_authorization_id: Option<String>,
    pub seller_revenue_cents: Cents,
    pub platform_fee_cents: Cents,
    pub platform_fee_usd: String,
    pub buyer_note: Option<String>,
    pub variant_key: Option<String>,
    /// The buyer's own label for the session the order belongs to, as set
    /// at creation ([`crate::CreateOrder::client_session_id`]).
    pub client_session_id: Option<String>,
    pub order_url: String,
    pub delivered_content_type: Option<String>,
    pub tracking_number: Option<String>,
    pub carrier: Option<String>,
    pub tracking_url: Option<String>,
    pub created_at: String,
    pub paid_at: Option<String>,
    pub delivered_at: Option<String>,
    pub expected_delivered_at: Option<String>,
    pub delivery_skew_seconds: Option<i64>,
    pub settlement_tx_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listing: Option<OrderListingRef>,

    // ---- detail only ----
    /// The deliverable inline. Absent when offloaded to `delivered_content_url`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "super::double_option"
    )]
    pub delivered_content: Option<Option<String>>,
    /// Where an offloaded deliverable can be downloaded (no auth needed);
    /// see [`crate::Client::fetch_delivered_content`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_content_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_content_byte_size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_content_filename: Option<String>,
    /// Sequence of the latest streamed chunk, while the seller streams.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partial_seq: Option<u64>,
    /// A streaming seller's output so far.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partial_content: Option<String>,

    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Order {
    /// The inline deliverable, if any.
    #[must_use]
    pub fn delivered_content(&self) -> Option<&str> {
        self.delivered_content.as_ref()?.as_deref()
    }

    /// Delivered, failed, cancelled or rejected: nothing more will happen.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        self.fulfillment_status.is_terminal()
    }
}

impl FulfillmentStatus {
    /// Statuses after which an order never changes again.
    pub const TERMINAL: &'static [FulfillmentStatus] = &[
        FulfillmentStatus::Delivered,
        FulfillmentStatus::Failed,
        FulfillmentStatus::Cancelled,
        FulfillmentStatus::Rejected,
    ];

    #[must_use]
    pub fn is_terminal(&self) -> bool {
        Self::TERMINAL.contains(self)
    }
}

/// Why a seller rejected a paid order.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rejection {
    pub reason_code: RejectionReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_cost_cents: Option<Cents>,
    /// The authorization the seller would accept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_authorization_cents: Option<Cents>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderListingRef {
    pub id: String,
    pub title: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "super::double_option"
    )]
    pub delivered_content_schema: Option<Option<Value>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// How paying a new order with `pay: merchant_credits` went.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaymentOutcome {
    pub status: PaymentOutcomeStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount_redeemed_cents: Option<Cents>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl PaymentOutcome {
    /// Fully paid now, or already paid before.
    #[must_use]
    pub fn is_settled(&self) -> bool {
        matches!(
            self.status,
            PaymentOutcomeStatus::FullyPaid | PaymentOutcomeStatus::AlreadyPaid
        )
    }
}

/// `POST /api/v1/orders`'s body: the order and, with `pay`, how paying went.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreatedOrder {
    #[serde(rename = "data")]
    pub order: Order,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payment: Option<PaymentOutcome>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

crate::response::data_envelope!(Order);
crate::response::whole_body!(CreatedOrder);
