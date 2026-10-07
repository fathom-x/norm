use serde_json::json;

use crate::auth::Auth;
use crate::client::{Client, Request};
use crate::error::Error;
use crate::models::{Account, BuyerRegistration, BuyerRegistrationRequest, WebSession};
use crate::response::Response;

/// `GET /api/v1/account`.
#[derive(Debug, Clone, Copy)]
pub struct AccountApi<'a>(pub(crate) &'a Client);

impl AccountApi<'_> {
    pub async fn get(&self, auth: Auth<'_>) -> Result<Response<Account>, Error> {
        Ok(Response::new(
            self.0
                .execute(Request::get("/api/v1/account"), auth)
                .await?,
        ))
    }
}

/// Buyer registration and browser sessions.
#[derive(Debug, Clone, Copy)]
pub struct BuyerApi<'a>(pub(crate) &'a Client);

impl BuyerApi<'_> {
    /// Find or create the buyer for a Nostr key and mint an API token.
    /// `auth` must be [`Auth::Nip98`].
    pub async fn register(
        &self,
        request: &BuyerRegistrationRequest,
        auth: Auth<'_>,
    ) -> Result<Response<BuyerRegistration>, Error> {
        let request = Request::post("/api/v1/buyer/register").json(serde_json::to_value(request)?);
        Ok(Response::new(self.0.execute(request, auth).await?))
    }

    /// A one-time browser login link for the token's account.
    pub async fn web_session(&self, auth: Auth<'_>) -> Result<Response<WebSession>, Error> {
        let request = Request::post("/api/v1/buyer/web_session").json(json!({}));
        Ok(Response::new(self.0.execute(request, auth).await?))
    }
}
