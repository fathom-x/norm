use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::common::Cents;
use crate::generated::PricingMode;

/// A listing. The index (`listings().list`) carries the summary fields with
/// a truncated description; `listings().get` adds the detail fields.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Listing {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub price_cents: Cents,
    pub price_usd: String,
    pub free: bool,
    pub currency: Option<String>,
    pub category: Option<String>,
    pub condition: Option<String>,
    /// -1 means unlimited.
    pub quantity: Option<i64>,
    pub main_image_url: Option<String>,
    pub delivered_content_type: Option<String>,
    pub seller: SellerRef,
    pub checkout_url: String,
    pub delivery_eta: Option<DeliveryEta>,
    /// Set when the listing is offered as a model-callable tool.
    pub provider_tool: Option<ProviderTool>,
    pub published_at: Option<String>,
    pub created_at: String,

    // ---- detail only ----
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "super::double_option"
    )]
    pub checkout_schema: Option<Option<Value>>,
    /// JSON Schema of the buyer_note; send the note as a JSON string.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "super::double_option"
    )]
    pub buyer_note_schema: Option<Option<Value>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "super::double_option"
    )]
    pub delivered_content_schema: Option<Option<Value>>,
    /// The buyer_note key that implies a variant (e.g. `"model"`).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "super::double_option"
    )]
    pub variant_field: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing_mode: Option<PricingMode>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "super::double_option"
    )]
    pub min_authorization_cents: Option<Option<i64>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "super::double_option"
    )]
    pub max_authorization_cents: Option<Option<i64>>,
    /// Active variants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variants: Option<Vec<ListingVariant>>,

    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Listing {
    /// The buyer_note schema, when the listing declares a non-empty one.
    #[must_use]
    pub fn buyer_note_schema(&self) -> Option<&Value> {
        self.buyer_note_schema
            .as_ref()?
            .as_ref()
            .filter(|s| !is_empty_object(s))
    }

    #[must_use]
    pub fn is_metered(&self) -> bool {
        self.pricing_mode == Some(PricingMode::Metered)
    }

    #[must_use]
    pub fn variant(&self, key: &str) -> Option<&ListingVariant> {
        self.variants.as_ref()?.iter().find(|v| v.key == key)
    }

    /// The authorization bounds for an order (in whole cents), with a
    /// variant's own bounds overriding the listing's per side.
    #[must_use]
    pub fn authorization_bounds(&self, variant: Option<&str>) -> (Option<i64>, Option<i64>) {
        let listing = (
            self.min_authorization_cents.flatten(),
            self.max_authorization_cents.flatten(),
        );
        match variant.and_then(|k| self.variant(k)) {
            Some(v) => (
                v.min_authorization_cents.or(listing.0),
                v.max_authorization_cents.or(listing.1),
            ),
            None => listing,
        }
    }

    /// `cents` clamped into [`Self::authorization_bounds`] (never below 1).
    #[must_use]
    pub fn clamp_authorization(&self, variant: Option<&str>, cents: i64) -> i64 {
        let (min, max) = self.authorization_bounds(variant);
        let mut cents = cents.max(min.unwrap_or(1)).max(1);
        if let Some(max) = max {
            cents = cents.min(max);
        }
        cents
    }
}

fn is_empty_object(value: &Value) -> bool {
    value.as_object().is_some_and(Map::is_empty)
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SellerRef {
    pub name: String,
    pub slug: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeliveryEta {
    pub p50_seconds: i64,
    pub p90_seconds: Option<i64>,
    pub source: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderTool {
    /// The function name the listing is offered under.
    pub name: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// One purchasable option of a listing (e.g. a model).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListingVariant {
    pub key: String,
    pub title: Option<String>,
    /// Seller-published and opaque to the marketplace.
    pub rate_card: Option<Value>,
    pub min_authorization_cents: Option<i64>,
    pub max_authorization_cents: Option<i64>,
    pub metadata: Option<Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A seller's public profile.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Seller {
    pub name: String,
    pub slug: String,
    pub wallets: Vec<SellerWallet>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SellerWallet {
    pub crypto_currency: String,
    pub crypto_address: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

crate::response::data_envelope!(Listing, Seller);
