use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::common::{Cents, DecimalString};
use crate::generated::{CreditTransactionType, PaymentStatus, RedemptionStatus};

/// A credit balance, held by a seller or by an organization.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "holder_type", rename_all = "snake_case")]
pub enum MerchantCredit {
    Seller(SellerHeldCredit),
    Organization(OrganizationHeldCredit),
}

impl MerchantCredit {
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::Seller(c) => &c.id,
            Self::Organization(c) => &c.id,
        }
    }

    #[must_use]
    pub fn balance_cents(&self) -> i64 {
        match self {
            Self::Seller(c) => c.balance_cents,
            Self::Organization(c) => c.balance_cents,
        }
    }

    /// Core marketplace credits — the primary spend balance.
    #[must_use]
    pub fn is_core(&self) -> bool {
        matches!(self, Self::Organization(c) if c.core)
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SellerHeldCredit {
    pub id: String,
    pub seller_slug: String,
    pub seller_name: Option<String>,
    pub balance_cents: i64,
    pub formatted_balance: String,
    pub total_purchased_cents: i64,
    pub total_redeemed_cents: i64,
    pub updated_at: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrganizationHeldCredit {
    pub id: String,
    pub organization_slug: String,
    pub organization_name: Option<String>,
    pub core: bool,
    pub balance_cents: i64,
    pub formatted_balance: String,
    pub total_purchased_cents: i64,
    pub total_redeemed_cents: i64,
    pub updated_at: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// The balance spendable at one seller.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MerchantCreditBalance {
    pub seller_slug: String,
    pub balance_cents: i64,
    pub formatted_balance: String,
    /// The organization whose balance this is, when the seller is in one.
    pub organization: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// One movement on a credit ledger.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreditTransaction {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: CreditTransactionType,
    pub created_at: String,
    /// Always positive; `kind` gives the direction.
    pub amount_cents: i64,
    pub balance_before_cents: i64,
    pub balance_after_cents: i64,
    pub credit_id: String,
    pub holder_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seller_slug: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_slug: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub core: Option<bool>,
    pub order_id: Option<String>,
    pub order_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redemption_id: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A Lightning invoice that loads core credits once paid.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreditLoad {
    /// The credit-purchase order; poll it until paid.
    pub order_id: String,
    pub bolt11: String,
    pub payment_hash: String,
    pub sats: i64,
    pub amount_cents: i64,
    pub expires_at: String,
    pub order_url: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A pending order that buys credits from a seller.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreditPurchase {
    pub order_id: String,
    pub order_type: String,
    pub total_usd_cents: DecimalString,
    pub payment_status: PaymentStatus,
    pub payment_expires_at: Option<String>,
    pub order_url: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub beneficiary_organization: Option<String>,
    /// The seller's USDC address, when it has a USDC wallet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payment_address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payment_amount_usdc: Option<serde_json::Number>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// How redeeming credits against an order went.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Redemption {
    pub status: RedemptionStatus,
    pub amount_redeemed_cents: Cents,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credit_balance_cents: Option<i64>,
    pub message: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Redemption {
    /// Fully paid now, or already paid before.
    #[must_use]
    pub fn is_settled(&self) -> bool {
        matches!(
            self.status,
            RedemptionStatus::FullyPaid | RedemptionStatus::AlreadyPaid
        )
    }
}

crate::response::data_envelope!(
    MerchantCreditBalance,
    CreditLoad,
    CreditPurchase,
    Redemption
);
