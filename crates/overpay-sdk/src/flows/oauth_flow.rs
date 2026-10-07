//! The OAuth authorization-code + PKCE flow, minus the browser.

use url::Url;

use crate::error::Error;
use crate::models::{OAuthClientRequest, OAuthToken};
use crate::pkce::Pkce;
use crate::Client;

/// One run of the browser login: register a client, send the user to
/// [`PkceFlow::authorize_url`], and once the browser comes back to
/// `redirect_uri` with `code` and `state`, [`PkceFlow::finish`] trades them
/// for an API token. Listening on the redirect URI is the caller's job.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct PkceFlow {
    pub client_id: String,
    pub redirect_uri: String,
    pub pkce: Pkce,
    pub authorize_url: Url,
}

/// Why finishing a login failed.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum OAuthFlowError {
    /// The callback's `state` isn't this flow's (CSRF, or a stale tab).
    #[error("OAuth state mismatch")]
    StateMismatch,
    #[error(transparent)]
    Api(#[from] Error),
}

impl PkceFlow {
    /// Register `client_name` for `redirect_uri` (a loopback address) and
    /// build the authorize URL for `scope`.
    pub async fn start(
        client: &Client,
        client_name: &str,
        redirect_uri: &str,
        scope: &str,
    ) -> Result<Self, Error> {
        let registration = OAuthClientRequest {
            client_name: client_name.to_string(),
            redirect_uris: vec![redirect_uri.to_string()],
            grant_types: vec!["authorization_code".into()],
            response_types: vec!["code".into()],
            scope: Some(scope.to_string()),
            token_endpoint_auth_method: Some("none".into()),
        };
        let client_id = client
            .oauth()
            .register_client(&registration)
            .await?
            .parse()?
            .client_id;
        Self::resume(client, client_id, redirect_uri, scope, Pkce::generate())
    }

    /// A flow for an already-registered client.
    pub fn resume(
        client: &Client,
        client_id: String,
        redirect_uri: &str,
        scope: &str,
        pkce: Pkce,
    ) -> Result<Self, Error> {
        let authorize_url = client.oauth().authorize_url(
            &client_id,
            redirect_uri,
            &pkce.state,
            &pkce.challenge,
            scope,
        )?;
        Ok(Self {
            client_id,
            redirect_uri: redirect_uri.to_string(),
            pkce,
            authorize_url,
        })
    }

    /// Exchange the callback's `code` for a token, after checking `state`.
    pub async fn finish(
        &self,
        client: &Client,
        code: &str,
        state: &str,
    ) -> Result<OAuthToken, OAuthFlowError> {
        if state != self.pkce.state {
            return Err(OAuthFlowError::StateMismatch);
        }
        let token = client
            .oauth()
            .exchange_code(
                &self.client_id,
                code,
                &self.pkce.verifier,
                &self.redirect_uri,
            )
            .await?;
        Ok(token.parse()?)
    }
}
