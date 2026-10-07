use serde_json::{json, Map, Value};

use crate::auth::Auth;
use crate::client::{Client, Request};
use crate::error::Error;
use crate::models::SpendAuthorization;
use crate::response::Response;

use super::segment;

/// A new session spending cap.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateSpendAuthorization {
    /// A metered listing.
    pub listing_id: String,
    /// At most $100 (10,000).
    pub cap_cents: i64,
    /// 60–86,400 seconds; the API's default is an hour.
    pub expires_in_seconds: Option<i64>,
}

impl CreateSpendAuthorization {
    /// A cap of `cap_cents` on `listing_id`, for the API's default hour.
    #[must_use]
    pub fn new(listing_id: impl Into<String>, cap_cents: i64) -> Self {
        Self {
            listing_id: listing_id.into(),
            cap_cents,
            expires_in_seconds: None,
        }
    }

    /// Expire after `seconds` (60–86,400).
    #[must_use]
    pub fn expires_in(mut self, seconds: i64) -> Self {
        self.expires_in_seconds = Some(seconds);
        self
    }

    fn body(&self) -> Value {
        let mut body = Map::new();
        body.insert("listing_id".into(), json!(self.listing_id));
        body.insert("cap_cents".into(), json!(self.cap_cents));
        if let Some(seconds) = self.expires_in_seconds {
            body.insert("expires_in_seconds".into(), json!(seconds));
        }
        Value::Object(body)
    }
}

/// Spend authorizations: open, read, close.
#[derive(Debug, Clone, Copy)]
pub struct SpendAuthorizationsApi<'a>(pub(crate) &'a Client);

impl SpendAuthorizationsApi<'_> {
    pub async fn create(
        &self,
        new: &CreateSpendAuthorization,
        auth: Auth<'_>,
    ) -> Result<Response<SpendAuthorization>, Error> {
        let request = Request::post("/api/v1/spend_authorizations").json(new.body());
        Ok(Response::new(self.0.execute(request, auth).await?))
    }

    pub async fn get(
        &self,
        id: &str,
        auth: Auth<'_>,
    ) -> Result<Response<SpendAuthorization>, Error> {
        let request = Request::get(format!("/api/v1/spend_authorizations/{}", segment(id)));
        Ok(Response::new(self.0.execute(request, auth).await?))
    }

    /// Close it (idempotent).
    pub async fn close(
        &self,
        id: &str,
        auth: Auth<'_>,
    ) -> Result<Response<SpendAuthorization>, Error> {
        let request = Request::post(format!(
            "/api/v1/spend_authorizations/{}/close",
            segment(id)
        ));
        Ok(Response::new(self.0.execute(request, auth).await?))
    }
}
