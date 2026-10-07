use serde_json::json;

use crate::auth::Auth;
use crate::client::{Client, Request};
use crate::error::Error;
use crate::models::{
    CreditLoad, CreditPurchase, CreditTransaction, MerchantCredit, MerchantCreditBalance, Page,
    Redemption,
};
use crate::response::Response;

use super::segment;

/// Filters for [`CreditsApi::transactions`].
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransactionQuery {
    /// Only movements at or after this ISO 8601 time.
    pub since: Option<String>,
    pub credit_id: Option<String>,
    /// Page size (the API caps it at 100).
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

/// Merchant credits: balances, the ledger, loading, buying, redeeming.
#[derive(Debug, Clone, Copy)]
pub struct CreditsApi<'a>(pub(crate) &'a Client);

impl CreditsApi<'_> {
    /// Every credit with a balance.
    pub async fn list(&self, auth: Auth<'_>) -> Result<Response<Vec<MerchantCredit>>, Error> {
        Ok(Response::new(
            self.0
                .execute(Request::get("/api/v1/merchant_credits"), auth)
                .await?,
        ))
    }

    /// The balance spendable at one seller.
    pub async fn get(
        &self,
        seller_slug: &str,
        auth: Auth<'_>,
    ) -> Result<Response<MerchantCreditBalance>, Error> {
        let request = Request::get(format!("/api/v1/merchant_credits/{}", segment(seller_slug)));
        Ok(Response::new(self.0.execute(request, auth).await?))
    }

    /// One page of the ledger (loads, spends, refunds), newest first.
    pub async fn transactions(
        &self,
        query: &TransactionQuery,
        auth: Auth<'_>,
    ) -> Result<Response<Page<CreditTransaction>>, Error> {
        let request = Request::get("/api/v1/merchant_credits/transactions")
            .query("since", query.since.as_deref())
            .query("credit_id", query.credit_id.as_deref())
            .query("limit", query.limit)
            .query("cursor", query.cursor.as_deref());
        Ok(Response::new(self.0.execute(request, auth).await?))
    }

    /// A Lightning invoice that loads `amount_cents` of core credits once paid.
    pub async fn load(
        &self,
        amount_cents: i64,
        auth: Auth<'_>,
    ) -> Result<Response<CreditLoad>, Error> {
        let request = Request::post("/api/v1/merchant_credits/load")
            .json(json!({ "amount_cents": amount_cents }));
        Ok(Response::new(self.0.execute(request, auth).await?))
    }

    /// A pending order that buys `amount_cents` of credits from a seller.
    pub async fn purchase(
        &self,
        seller_slug: &str,
        amount_cents: i64,
        auth: Auth<'_>,
    ) -> Result<Response<CreditPurchase>, Error> {
        let request = Request::post(format!(
            "/api/v1/merchant_credits/{}/purchase",
            segment(seller_slug)
        ))
        .json(json!({ "amount_cents": amount_cents }));
        Ok(Response::new(self.0.execute(request, auth).await?))
    }

    /// Pay a pending order with credits.
    pub async fn redeem(
        &self,
        seller_slug: &str,
        order_id: &str,
        auth: Auth<'_>,
    ) -> Result<Response<Redemption>, Error> {
        let request = Request::post(format!(
            "/api/v1/merchant_credits/{}/redeem",
            segment(seller_slug)
        ))
        .json(json!({ "order_id": order_id }));
        Ok(Response::new(self.0.execute(request, auth).await?))
    }
}
