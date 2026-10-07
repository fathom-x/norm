use url::Url;

use crate::auth::Auth;
use crate::client::{Client, Request};
use crate::error::Error;
use crate::models::{OAuthClient, OAuthClientRequest, OAuthServerMetadata, OAuthToken};
use crate::pkce::Pkce;
use crate::response::Response;

/// OAuth 2.0 authorization code + PKCE: how a wallet gets an API token
/// through the user's browser. See [`crate::flows::PkceFlow`] for the
/// whole flow.
#[derive(Debug, Clone, Copy)]
pub struct OAuthApi<'a>(pub(crate) &'a Client);

impl OAuthApi<'_> {
    pub async fn metadata(&self) -> Result<Response<OAuthServerMetadata>, Error> {
        Ok(Response::new(
            self.0
                .execute(
                    Request::get("/.well-known/oauth-authorization-server"),
                    Auth::None,
                )
                .await?,
        ))
    }

    /// Dynamic client registration (public clients; no secret).
    pub async fn register_client(
        &self,
        request: &OAuthClientRequest,
    ) -> Result<Response<OAuthClient>, Error> {
        let mut request = Request::post("/oauth/clients").json(serde_json::to_value(request)?);
        request.accept_json = false;
        Ok(Response::new(self.0.execute(request, Auth::None).await?))
    }

    /// The browser URL that asks the user to approve the client. It is on
    /// the marketplace's public URL.
    pub fn authorize_url(
        &self,
        client_id: &str,
        redirect_uri: &str,
        state: &str,
        code_challenge: &str,
        scope: &str,
    ) -> Result<Url, Error> {
        let mut url = self.0.join_public("/oauth/authorize")?;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("scope", scope)
            .append_pair("state", state)
            .append_pair("code_challenge", code_challenge)
            .append_pair("code_challenge_method", Pkce::method());
        Ok(url)
    }

    /// Exchange an authorization code and its PKCE verifier for a token.
    pub async fn exchange_code(
        &self,
        client_id: &str,
        code: &str,
        verifier: &str,
        redirect_uri: &str,
    ) -> Result<Response<OAuthToken>, Error> {
        let fields = [
            ("grant_type", "authorization_code"),
            ("client_id", client_id),
            ("code", code),
            ("code_verifier", verifier),
            ("redirect_uri", redirect_uri),
        ];
        let mut request = Request::post("/oauth/token").form(
            fields
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
        );
        request.accept_json = false;
        Ok(Response::new(self.0.execute(request, Auth::None).await?))
    }
}
