//! The API's operations, grouped by resource: `client.orders().get(..)`.

mod account;
mod credits;
mod listings;
mod oauth;
mod orders;
mod spend;

pub use account::{AccountApi, BuyerApi};
pub use credits::{CreditsApi, TransactionQuery};
pub use listings::{ListingQuery, ListingsApi, SellersApi};
pub use oauth::OAuthApi;
pub use orders::{CreateOrder, GetOrder, OrderQuery, OrdersApi};
pub use spend::{CreateSpendAuthorization, SpendAuthorizationsApi};

use crate::Client;

impl Client {
    #[must_use]
    pub fn listings(&self) -> ListingsApi<'_> {
        ListingsApi(self)
    }

    #[must_use]
    pub fn sellers(&self) -> SellersApi<'_> {
        SellersApi(self)
    }

    #[must_use]
    pub fn orders(&self) -> OrdersApi<'_> {
        OrdersApi(self)
    }

    #[must_use]
    pub fn credits(&self) -> CreditsApi<'_> {
        CreditsApi(self)
    }

    #[must_use]
    pub fn spend_authorizations(&self) -> SpendAuthorizationsApi<'_> {
        SpendAuthorizationsApi(self)
    }

    #[must_use]
    pub fn account(&self) -> AccountApi<'_> {
        AccountApi(self)
    }

    #[must_use]
    pub fn buyer(&self) -> BuyerApi<'_> {
        BuyerApi(self)
    }

    #[must_use]
    pub fn oauth(&self) -> OAuthApi<'_> {
        OAuthApi(self)
    }
}

/// Percent-encode one path segment.
pub(crate) fn segment(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes())
        .collect::<String>()
        .replace('+', "%20")
}
