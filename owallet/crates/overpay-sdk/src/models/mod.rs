//! Typed views of the API's JSON.
//!
//! Every model keeps the fields it doesn't name in `extra`, so parsing and
//! re-serializing a response gives back the same JSON — a typed model never
//! drops something the API sent. Fields the API omits in some responses
//! (detail-only fields) are `Option` and skipped when absent; fields the
//! API always sends but may set to null are plain `Option`. The few that
//! are both are `Option<Option<_>>`: absent vs. null.

mod account;
mod common;
mod credits;
mod listing;
mod oauth;
mod order;
mod spend;

pub use account::{Account, BuyerRegistration, BuyerRegistrationRequest, WebSession};
pub use common::{Cents, DecimalString, Page};
pub use credits::{
    CreditLoad, CreditPurchase, CreditTransaction, MerchantCredit, MerchantCreditBalance,
    OrganizationHeldCredit, Redemption, SellerHeldCredit,
};
pub use listing::{
    DeliveryEta, Listing, ListingVariant, ProviderTool, Seller, SellerRef, SellerWallet,
};
pub use oauth::{OAuthClient, OAuthClientRequest, OAuthServerMetadata, OAuthToken};
pub use order::{CreatedOrder, Order, OrderListingRef, PaymentOutcome, Rejection};
pub use spend::SpendAuthorization;

/// `Option<Option<T>>` that tells an absent field (`None`) from a null one
/// (`Some(None)`), for `#[serde(default, with = "double_option")]`.
pub(crate) mod double_option {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn deserialize<'de, T: Deserialize<'de>, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<Option<T>>, D::Error> {
        Option::<T>::deserialize(d).map(Some)
    }

    #[allow(clippy::ref_option, clippy::option_option)]
    pub fn serialize<T: Serialize, S: Serializer>(
        value: &Option<Option<T>>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(inner) => inner.serialize(s),
            None => s.serialize_none(),
        }
    }
}
