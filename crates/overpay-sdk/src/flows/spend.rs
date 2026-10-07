//! Spend sessions: many metered orders under one spending cap.

use crate::auth::Auth;
use crate::error::Error;
use crate::models::{CreatedOrder, SpendAuthorization};
use crate::resources::{CreateOrder, CreateSpendAuthorization};
use crate::response::Response;
use crate::Client;

/// A spend authorization in use: open it, place each turn's order under
/// it, close it when done. The marketplace sizes each turn's authorization
/// to what's left of the cap (at most the variant's per-turn maximum) and
/// the seller captures actual cost, so closing never has money to return.
///
/// ```no_run
/// # async fn demo(client: overpay_sdk::Client) -> Result<(), overpay_sdk::Error> {
/// use overpay_sdk::{flows::SpendSession, Auth};
///
/// let auth = Auth::Bearer("token");
/// let session = SpendSession::open(&client, "listing-id", 500, None, auth).await?;
/// let turn = session.order(Some(r#"{"model":"default","messages":[]}"#), auth).await?;
/// println!("order {}", turn.parse()?.order.id);
/// session.close(auth).await?;
/// # Ok(()) }
/// ```
pub struct SpendSession<'a> {
    client: &'a Client,
    authorization: SpendAuthorization,
}

impl<'a> SpendSession<'a> {
    /// Open a cap of `cap_cents` on a metered listing.
    pub async fn open(
        client: &'a Client,
        listing_id: &str,
        cap_cents: i64,
        expires_in_seconds: Option<i64>,
        auth: Auth<'_>,
    ) -> Result<Self, Error> {
        let new = CreateSpendAuthorization {
            listing_id: listing_id.to_string(),
            cap_cents,
            expires_in_seconds,
        };
        let authorization = client
            .spend_authorizations()
            .create(&new, auth)
            .await?
            .parse()?;
        Ok(Self {
            client,
            authorization,
        })
    }

    /// The authorization as last read.
    #[must_use]
    pub fn authorization(&self) -> &SpendAuthorization {
        &self.authorization
    }

    /// Place (and pay for) one order under the cap. An exhausted cap is
    /// 422 `authorization_exhausted`, with `remaining_cents`.
    pub async fn order(
        &self,
        buyer_note: Option<&str>,
        auth: Auth<'_>,
    ) -> Result<Response<CreatedOrder>, Error> {
        let mut order = CreateOrder::new(&self.authorization.listing_id)
            .spend_authorization(&self.authorization.id);
        order.buyer_note = buyer_note.map(str::to_string);
        self.client.orders().create(&order, auth).await
    }

    /// Re-read what's spent and what's left.
    pub async fn refresh(&mut self, auth: Auth<'_>) -> Result<&SpendAuthorization, Error> {
        self.authorization = self
            .client
            .spend_authorizations()
            .get(&self.authorization.id, auth)
            .await?
            .parse()?;
        Ok(&self.authorization)
    }

    pub async fn close(self, auth: Auth<'_>) -> Result<SpendAuthorization, Error> {
        self.client
            .spend_authorizations()
            .close(&self.authorization.id, auth)
            .await?
            .parse()
    }
}
