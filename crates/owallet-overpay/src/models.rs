//! Wire types for the Overpay Rails API.
//!
//! All shapes are `#[serde(default)]` where reasonable so that schema
//! additions on the Rails side don't break the Rust client.
//!
//! ## JSONAPI envelope
//!
//! Most Rails responses wrap the payload in `{"data": {...}}` (see
//! `Api::V1::BaseController#render_json` plus the manual envelope in
//! `orders_controller.rb#show` / `#create`). The `unwrap_data_envelope`
//! helper transparently unwraps it so the typed structs can stay
//! flat — and we deliberately fall back to the bare object too so
//! tests, mocks, and any future Rails change that drops the envelope
//! still decode cleanly. This is the same `body.get("data", body)`
//! tolerance the Python client uses (`wallet_mcp/server.py:1606,1694`).

use serde::{Deserialize, Serialize};

/// If `v` is an object with a `"data"` key whose value is itself an
/// object or array, return that inner value. Otherwise return `v`
/// unchanged. Mirrors Python's `body.get("data", body)`.
fn unwrap_data_envelope(v: &serde_json::Value) -> &serde_json::Value {
    match v.get("data") {
        Some(inner) if inner.is_object() || inner.is_array() => inner,
        _ => v,
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OAuthTokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub token_type: Option<String>,
    #[serde(default)]
    pub expires_in: Option<i64>,
    #[serde(default)]
    pub scope: Option<String>,
}

/// Buyer account information returned by `GET /api/v1/account`.
/// The Rails endpoint wraps the payload in a JSONAPI-style envelope
/// (`{"data": {...}}`); the custom `Deserialize` unwraps it transparently
/// and also accepts a bare object for forward/backward compatibility.
/// `account_number` carries `formatted_account_number` (the dashed
/// 16-digit display form) when present, falling back to the raw
/// `account_number` field.
#[derive(Debug, Clone, Serialize, Default)]
pub struct AccountInfo {
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub account_number: Option<String>,
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default)]
    pub npub: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
}

impl<'de> Deserialize<'de> for AccountInfo {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = serde_json::Value::deserialize(d)?;
        let inner = unwrap_data_envelope(&v);
        Ok(AccountInfo {
            username: opt_string(inner, "username"),
            // Prefer the dashed display form when Rails sends it
            // (e.g. "1234-5678-9012-3456"); fall back to the raw value.
            account_number: opt_string(inner, "formatted_account_number")
                .or_else(|| opt_string(inner, "account_number")),
            address: opt_string(inner, "address"),
            npub: opt_string(inner, "npub"),
            email: opt_string(inner, "email"),
        })
    }
}

/// Read a string field from a JSON object, returning `None` for missing
/// or non-string values (including `null`).
fn opt_string(v: &serde_json::Value, k: &str) -> Option<String> {
    v.get(k).and_then(|v| v.as_str()).map(str::to_string)
}

/// Read an integer-shaped field, accepting either a JSON number or a
/// numeric string. `None` for missing / null / non-numeric values.
fn opt_i64(v: &serde_json::Value, k: &str) -> Option<i64> {
    match v.get(k)? {
        serde_json::Value::Number(n) => n.as_i64(),
        serde_json::Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

/// Read a float-shaped field, accepting either a JSON number or a
/// numeric string. `None` for missing / null / non-numeric values.
fn opt_f64(v: &serde_json::Value, k: &str) -> Option<f64> {
    match v.get(k)? {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

/// One-shot web-login URL the buyer can visit to land on Overpay
/// already authenticated. The Rails endpoint returns one of:
///
/// - `{"login_url": "...", "expires_at": "..."}` — current shape
/// - `{"data": {"login_url": "...", ...}}` — JSONAPI-style envelope
/// - `{"url": "..."}` — legacy / mock shape
///
/// `deserialize_url` accepts all three so the client doesn't break the
/// minute the Rails side changes one or the other.
#[derive(Debug, Clone, Serialize)]
pub struct WebSessionResponse {
    pub url: String,
}

impl<'de> Deserialize<'de> for WebSessionResponse {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let v = serde_json::Value::deserialize(d)?;
        let inner = unwrap_data_envelope(&v);
        let url = opt_string(inner, "login_url")
            .or_else(|| opt_string(inner, "url"))
            .ok_or_else(|| {
                D::Error::custom("web_session response missing both `login_url` and `url`")
            })?;
        Ok(Self { url })
    }
}

/// Order summary returned by the buyer order endpoints. The Rails
/// `orders_controller#order_json` emits `payment_status` /
/// `tracking_number`, formats `total_usd` as a `"$0.12"` string, and
/// nests the listing reference under `listing.id` rather than a flat
/// `listing_id`. The hand-written `Deserialize` accepts either the
/// `{data: {...}}` envelope used by `show`/`create` or the bare object
/// shape that appears inside the `index` array.
#[derive(Debug, Clone, Serialize, Default)]
pub struct Order {
    pub id: String,
    #[serde(default)]
    pub payment_status: Option<String>,
    #[serde(default)]
    pub fulfillment_status: Option<String>,
    #[serde(default)]
    pub listing_id: Option<String>,
    #[serde(default)]
    pub listing_title: Option<String>,
    /// Buyer-visible total formatted as a price string (e.g. `"$0.12"`,
    /// `"Free"`). Kept as a string because Rails emits it that way.
    #[serde(default)]
    pub total_usd: Option<String>,
    #[serde(default)]
    pub total_usd_cents: Option<f64>,
    #[serde(default)]
    pub product_title: Option<String>,
    #[serde(default)]
    pub buyer_note: Option<String>,
    #[serde(default)]
    pub order_url: Option<String>,
    #[serde(default)]
    pub tracking_number: Option<String>,
    #[serde(default)]
    pub tracking_url: Option<String>,
    #[serde(default)]
    pub carrier: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub paid_at: Option<String>,
    #[serde(default)]
    pub delivered_at: Option<String>,
    #[serde(default)]
    pub settlement_tx_hash: Option<String>,
}

impl<'de> Deserialize<'de> for Order {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let v = serde_json::Value::deserialize(d)?;
        let o = unwrap_data_envelope(&v);
        let id = opt_string(o, "id").ok_or_else(|| D::Error::custom("order missing `id`"))?;
        let listing = o.get("listing");
        Ok(Order {
            id,
            payment_status: opt_string(o, "payment_status"),
            fulfillment_status: opt_string(o, "fulfillment_status"),
            listing_id: listing
                .and_then(|l| opt_string(l, "id"))
                .or_else(|| opt_string(o, "listing_id")),
            listing_title: listing.and_then(|l| opt_string(l, "title")),
            total_usd: opt_string(o, "total_usd"),
            total_usd_cents: opt_f64(o, "total_usd_cents"),
            product_title: opt_string(o, "product_title"),
            buyer_note: opt_string(o, "buyer_note"),
            order_url: opt_string(o, "order_url"),
            tracking_number: opt_string(o, "tracking_number"),
            tracking_url: opt_string(o, "tracking_url"),
            carrier: opt_string(o, "carrier"),
            created_at: opt_string(o, "created_at"),
            paid_at: opt_string(o, "paid_at"),
            delivered_at: opt_string(o, "delivered_at"),
            settlement_tx_hash: opt_string(o, "settlement_tx_hash"),
        })
    }
}

/// One merchant-credit balance row. Rails emits two shapes
/// (`api/v1/merchant_credits_controller.rb#credit_json`):
///
/// - seller-owned: `{holder_type: "seller", seller_slug, seller_name, ...}`
/// - organization-owned: `{holder_type: "organization", organization_slug,
///   organization_name, ...}`
///
/// `seller_slug` is therefore optional, and we surface `organization_slug`
/// alongside it. The `show` endpoint always returns the seller-owned
/// shape inside a `{data: {...}}` envelope; `index` returns
/// `{data: [...]}` where each item is bare. The custom `Deserialize`
/// unwraps the envelope when present and accepts the bare object too.
#[derive(Debug, Clone, Serialize, Default)]
pub struct MerchantCredits {
    #[serde(default)]
    pub seller_slug: Option<String>,
    #[serde(default)]
    pub organization_slug: Option<String>,
    #[serde(default)]
    pub holder_type: Option<String>,
    /// Cents, to 4 places once sub-cent charges have touched the balance.
    #[serde(default)]
    pub balance_cents: Option<f64>,
    #[serde(default)]
    pub total_purchased_cents: Option<f64>,
    #[serde(default)]
    pub total_redeemed_cents: Option<f64>,
    #[serde(default)]
    pub currency: Option<String>,
    #[serde(default)]
    pub formatted_balance: Option<String>,
}

impl<'de> Deserialize<'de> for MerchantCredits {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = serde_json::Value::deserialize(d)?;
        let o = unwrap_data_envelope(&v);
        Ok(MerchantCredits {
            seller_slug: opt_string(o, "seller_slug"),
            organization_slug: opt_string(o, "organization_slug"),
            holder_type: opt_string(o, "holder_type"),
            balance_cents: opt_f64(o, "balance_cents"),
            total_purchased_cents: opt_f64(o, "total_purchased_cents"),
            total_redeemed_cents: opt_f64(o, "total_redeemed_cents"),
            currency: opt_string(o, "currency"),
            formatted_balance: opt_string(o, "formatted_balance"),
        })
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct MerchantCreditsList {
    #[serde(default)]
    pub data: Vec<MerchantCredits>,
}

/// Response from `POST /api/v1/merchant_credits/{slug}/purchase`. The
/// Rails controller wraps the body in a `{data: {...}}` envelope. The
/// `payment_address` / `payment_amount_usdc` fields are only present
/// when the seller has a USDC wallet on file — for non-USDC sellers
/// the buyer pays via a different rail and these are absent.
#[derive(Debug, Clone, Serialize, Default)]
pub struct PurchaseCreditsResponse {
    pub order_id: String,
    #[serde(default)]
    pub payment_address: Option<String>,
    #[serde(default)]
    pub payment_amount_usdc: Option<f64>,
    /// Settlement currency the server picked for this order (`"USDC"` /
    /// `"ZEC"`). Present once a payment row is created; absent when no rail
    /// is configured.
    #[serde(default)]
    pub currency: Option<String>,
    /// Crypto address for a non-USDC rail — for Zcash, the Orchard Unified
    /// Address to pay. Rails emits this as `crypto_address` on the payment.
    #[serde(default)]
    pub crypto_address: Option<String>,
    /// Generic payment amount in the settlement `currency` (ZEC when
    /// `currency == "ZEC"`). USDC orders also carry `payment_amount_usdc`.
    #[serde(default)]
    pub payment_amount: Option<f64>,
    #[serde(default)]
    pub total_usd_cents: Option<f64>,
    #[serde(default)]
    pub payment_status: Option<String>,
    #[serde(default)]
    pub order_url: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
}

impl PurchaseCreditsResponse {
    /// If the server picked Zcash for this order, return the
    /// `(orchard_ua, amount_zec)` to pay. Routes on a Zcash-shaped address
    /// (`currency == "ZEC"` or a UA in `crypto_address`/`payment_address`)
    /// so the buy flow can dispatch to the Zcash backend.
    #[must_use]
    pub fn zcash_payment(&self) -> Option<(String, f64)> {
        let is_zec = self
            .currency
            .as_deref()
            .is_some_and(|c| c.eq_ignore_ascii_case("zec"));
        let addr = self
            .crypto_address
            .clone()
            .or_else(|| self.payment_address.clone())?;
        // Only treat as Zcash when the currency says so, or the address
        // clearly isn't an EVM `0x…` address (i.e. looks like a UA).
        if !is_zec && addr.starts_with("0x") {
            return None;
        }
        let amount = self.payment_amount.or(self.payment_amount_usdc)?;
        Some((addr, amount))
    }
}

impl<'de> Deserialize<'de> for PurchaseCreditsResponse {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let v = serde_json::Value::deserialize(d)?;
        let o = unwrap_data_envelope(&v);
        let order_id =
            opt_string(o, "order_id").ok_or_else(|| D::Error::custom("missing `order_id`"))?;
        Ok(PurchaseCreditsResponse {
            order_id,
            payment_address: opt_string(o, "payment_address"),
            payment_amount_usdc: opt_f64(o, "payment_amount_usdc"),
            currency: opt_string(o, "currency"),
            crypto_address: opt_string(o, "crypto_address"),
            payment_amount: opt_f64(o, "payment_amount"),
            total_usd_cents: opt_f64(o, "total_usd_cents"),
            payment_status: opt_string(o, "payment_status"),
            order_url: opt_string(o, "order_url"),
            message: opt_string(o, "message"),
        })
    }
}

/// Response from `POST /api/v1/merchant_credits/load`. The Rails controller
/// returns bolt11, payment_hash, sats, amount_cents, expires_at, order_url.
#[derive(Debug, Clone, Serialize, Default)]
pub struct LightningLoadResponse {
    pub order_id: String,
    pub bolt11: String,
    pub payment_hash: String,
    pub sats: i64,
    pub amount_cents: i64,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub order_url: Option<String>,
}

impl<'de> Deserialize<'de> for LightningLoadResponse {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let v = serde_json::Value::deserialize(d)?;
        let o = unwrap_data_envelope(&v);
        Ok(LightningLoadResponse {
            order_id: opt_string(o, "order_id")
                .ok_or_else(|| D::Error::custom("missing `order_id`"))?,
            bolt11: opt_string(o, "bolt11").ok_or_else(|| D::Error::custom("missing `bolt11`"))?,
            payment_hash: opt_string(o, "payment_hash")
                .ok_or_else(|| D::Error::custom("missing `payment_hash`"))?,
            sats: opt_i64(o, "sats").unwrap_or(0),
            amount_cents: opt_i64(o, "amount_cents").unwrap_or(0),
            expires_at: opt_string(o, "expires_at"),
            order_url: opt_string(o, "order_url"),
        })
    }
}

/// Filters for the marketplace listings query.
#[derive(Debug, Clone, Default)]
pub struct ListingFilters {
    pub category: Option<String>,
    pub seller_slug: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

/// Filters for the buyer orders query. The Rails endpoint takes
/// `payment_status=` (not `status=`) — `orders_controller.rb#index`.
#[derive(Debug, Clone, Default)]
pub struct OrderFilters {
    pub payment_status: Option<String>,
    pub fulfillment_status: Option<String>,
    pub limit: Option<u32>,
    pub cursor: Option<String>,
    /// Required by Rails when authenticating via NIP-98: the
    /// `orders_controller#authorize_payer_address!` filter pins the
    /// requester's EVM address so the NIP-98 pubkey can be verified
    /// against it. Bearer-authenticated requests skip the check, and
    /// passing the address anyway widens the result set.
    pub payer_address: Option<String>,
    /// Only orders created with this `client_session_id` — the opaque
    /// label owallet puts on a session's orders. A marketplace that
    /// predates the field ignores the filter and returns every order, and
    /// its rows carry no `client_session_id` key: check for it.
    pub client_session_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn purchase_response_usdc_does_not_route_to_zcash() {
        let v = serde_json::json!({
            "order_id": "o1",
            "payment_address": "0xabc0000000000000000000000000000000000000",
            "payment_amount_usdc": 1.5,
        });
        let r: PurchaseCreditsResponse = serde_json::from_value(v).unwrap();
        assert_eq!(r.payment_amount_usdc, Some(1.5));
        assert!(r.zcash_payment().is_none());
    }

    #[test]
    fn purchase_response_zec_routes_to_zcash() {
        let v = serde_json::json!({
            "order_id": "o2",
            "currency": "ZEC",
            "crypto_address": "u1exampleorchardunifiedaddress",
            "payment_amount": 0.25,
        });
        let r: PurchaseCreditsResponse = serde_json::from_value(v).unwrap();
        let (ua, amt) = r.zcash_payment().expect("should route to zcash");
        assert_eq!(ua, "u1exampleorchardunifiedaddress");
        assert_eq!(amt, 0.25);
    }

    #[test]
    fn purchase_response_zec_via_data_envelope() {
        let v = serde_json::json!({
            "data": {
                "order_id": "o3",
                "currency": "ZEC",
                "crypto_address": "u1another",
                "payment_amount": 2.0,
            }
        });
        let r: PurchaseCreditsResponse = serde_json::from_value(v).unwrap();
        assert_eq!(r.order_id, "o3");
        assert_eq!(r.zcash_payment(), Some(("u1another".into(), 2.0)));
    }
}
