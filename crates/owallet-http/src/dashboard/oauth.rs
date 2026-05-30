//! Browser-initiated Overpay OAuth from the dashboard.
//!
//! Mirrors the three Python handlers in `wallet_mcp/server.py:732-846`:
//!
//! - `GET /wallet/overpay-login` — one-shot redirect into a server-issued
//!   web session URL (returned by `POST /api/v1/buyer/web_session`).
//!   Requires a stored Overpay bearer for the active wallet.
//! - `GET /wallet/authorize` — initiates a PKCE OAuth handshake against
//!   the Overpay Rails app. Registers a fresh public client, stores the
//!   pending state in a short-lived `DashMap`, sets the
//!   `owallet_pending_auth` cookie, and 302-redirects to Overpay's
//!   `/oauth/authorize`.
//! - `GET /wallet/authorize/callback` — exchanges the PKCE code, stores
//!   the resulting bearer under `(npub, host_key)`, best-effort refreshes
//!   the cached username, and redirects back to `/wallet?notice=authorized`.

use std::time::{Duration, Instant};

use axum::extract::{Query, State};
use axum::http::header::{COOKIE, LOCATION, SET_COOKIE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use cookie::{Cookie, SameSite};
use owallet_overpay::models::OAuthRegisterRequest;
use owallet_overpay::{Auth, Pkce};
use rand::RngCore;
use serde::Deserialize;

use super::{current_session, redirect_to_login};
use crate::error::AppError;
use crate::session::SessionRole;
use crate::state::{AppState, PendingDashboardAuth, PENDING_AUTH_COOKIE};

const PENDING_TTL: Duration = Duration::from_secs(300);

// ---------------------------------------------------------------------------
// GET /wallet/overpay-login
// ---------------------------------------------------------------------------

pub async fn overpay_login_get(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let Some(session) = current_session(&state.sessions, &headers) else {
        return Ok(redirect_to_login().into_response());
    };
    let Some(npub) = resolve_npub(&state, &session)? else {
        return Ok(Redirect::to("/wallet?notice=no-wallet").into_response());
    };

    let stored = {
        let db = state
            .db
            .lock()
            .map_err(|e| AppError::Internal(format!("db mutex poisoned: {e}")))?;
        db.read_token(&npub, &state.host_key)?
    };
    let Some(token) = stored else {
        return Ok(Redirect::to("/wallet?notice=run-authorize").into_response());
    };

    let session = state.overpay.web_session(Auth::Bearer(&token)).await?;
    let url = state.overpay.to_public_url(&session.url);
    Ok(Redirect::to(&url).into_response())
}

// ---------------------------------------------------------------------------
// GET /wallet/authorize
// ---------------------------------------------------------------------------

pub async fn authorize_get(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let Some(session) = current_session(&state.sessions, &headers) else {
        return Ok(redirect_to_login().into_response());
    };
    let Some(npub) = resolve_npub(&state, &session)? else {
        return Ok(Redirect::to("/wallet?notice=no-wallet").into_response());
    };

    let pkce = Pkce::generate();
    let redirect_uri = format!("{}/wallet/authorize/callback", state.host_key);

    let reg = state
        .overpay
        .register_oauth_client(&OAuthRegisterRequest {
            client_name: "owallet-dashboard".into(),
            redirect_uris: vec![redirect_uri.clone()],
            grant_types: vec!["authorization_code".into()],
            response_types: vec!["code".into()],
            scope: Some("wallet".into()),
            token_endpoint_auth_method: Some("none".into()),
        })
        .await?;

    let pending_id = rand_id();
    state.pending_auth.0.insert(
        pending_id.clone(),
        PendingDashboardAuth {
            pkce: pkce.clone(),
            client_id: reg.client_id.clone(),
            redirect_uri: redirect_uri.clone(),
            started_npub: npub,
            expires_at: Instant::now() + PENDING_TTL,
        },
    );

    let auth_url = state.overpay.authorize_url(
        &reg.client_id,
        &redirect_uri,
        &pkce.state,
        &pkce.challenge,
        "wallet",
    )?;

    Ok(redirect_with_cookie(
        auth_url.as_str(),
        pending_auth_set_cookie(&pending_id),
    ))
}

// ---------------------------------------------------------------------------
// GET /wallet/authorize/callback
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub error_description: Option<String>,
}

pub async fn authorize_callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<CallbackQuery>,
) -> Result<Response, AppError> {
    // Always clear the pending-auth cookie on the way back, whether or not
    // the exchange succeeds.
    let clear_cookie = pending_auth_clear_cookie();

    if let Some(err) = q.error {
        let msg = q.error_description.unwrap_or(err);
        return Ok(redirect_with_cookie(
            &format!("/wallet?notice=authorize-error&msg={}", urlenc(&msg)),
            clear_cookie,
        ));
    }

    let Some(pending_id) = read_pending_auth_cookie(&headers) else {
        return Ok(redirect_with_cookie(
            "/wallet?notice=authorize-error&msg=missing-cookie",
            clear_cookie,
        ));
    };

    let Some((_, pending)) = state.pending_auth.0.remove(&pending_id) else {
        return Ok(redirect_with_cookie(
            "/wallet?notice=authorize-error&msg=pending-expired",
            clear_cookie,
        ));
    };
    if Instant::now() >= pending.expires_at {
        return Ok(redirect_with_cookie(
            "/wallet?notice=authorize-error&msg=pending-expired",
            clear_cookie,
        ));
    }
    if q.state.as_deref() != Some(pending.pkce.state.as_str()) {
        return Ok(redirect_with_cookie(
            "/wallet?notice=authorize-error&msg=state-mismatch",
            clear_cookie,
        ));
    }
    let Some(code) = q.code else {
        return Ok(redirect_with_cookie(
            "/wallet?notice=authorize-error&msg=missing-code",
            clear_cookie,
        ));
    };

    let token = state
        .overpay
        .exchange_code(
            &pending.client_id,
            &code,
            &pending.pkce.verifier,
            &pending.redirect_uri,
        )
        .await?;

    {
        let db = state
            .db
            .lock()
            .map_err(|e| AppError::Internal(format!("db mutex poisoned: {e}")))?;
        db.write_token(
            &pending.started_npub,
            &state.host_key,
            &token.access_token,
            "overpay-oauth",
        )?;
    }

    // Best-effort: refresh the cached username so the dashboard shows the
    // link immediately. A failure here doesn't roll back the token store.
    if let Ok(info) = state
        .overpay
        .account(Auth::Bearer(&token.access_token))
        .await
    {
        if let Some(u) = info.username.as_deref() {
            let db = state
                .db
                .lock()
                .map_err(|e| AppError::Internal(format!("db mutex poisoned: {e}")))?;
            let _ = db.cache_wallet_username(&pending.started_npub, u);
        }
    }

    Ok(redirect_with_cookie(
        "/wallet?notice=authorized",
        clear_cookie,
    ))
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn resolve_npub(
    state: &AppState,
    session: &crate::session::WebSession,
) -> Result<Option<String>, AppError> {
    let db = state
        .db
        .lock()
        .map_err(|e| AppError::Internal(format!("db mutex poisoned: {e}")))?;
    match &session.role {
        SessionRole::Wallet { npub } => Ok(Some(npub.clone())),
        SessionRole::Admin => Ok(db.read_default_npub()?),
    }
}

fn rand_id() -> String {
    let mut bytes = [0u8; 24];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn pending_auth_set_cookie(value: &str) -> HeaderValue {
    let mut c = Cookie::new(PENDING_AUTH_COOKIE, value.to_string());
    c.set_path("/");
    c.set_http_only(true);
    c.set_same_site(SameSite::Lax);
    c.set_max_age(cookie::time::Duration::seconds(PENDING_TTL.as_secs() as i64));
    HeaderValue::from_str(&c.to_string()).expect("cookie")
}

fn pending_auth_clear_cookie() -> HeaderValue {
    let mut c = Cookie::new(PENDING_AUTH_COOKIE, "");
    c.set_path("/");
    c.set_max_age(cookie::time::Duration::ZERO);
    HeaderValue::from_str(&c.to_string()).expect("cookie")
}

fn read_pending_auth_cookie(headers: &HeaderMap) -> Option<String> {
    // Browsers send all cookies in a single `Cookie:` header; axum-test
    // sends one header per cookie. Be robust to both.
    for raw in headers.get_all(COOKIE).iter() {
        let Ok(s) = raw.to_str() else {
            continue;
        };
        for c in Cookie::split_parse(s).flatten() {
            if c.name() == PENDING_AUTH_COOKIE {
                return Some(c.value().to_string());
            }
        }
    }
    None
}

/// Build a 303 See Other to `target` with a single Set-Cookie attached.
/// Avoids `Redirect::to(...).into_response().headers_mut().extend(...)`
/// dance — direct construction keeps the Set-Cookie header definitely
/// present in the wire response.
fn redirect_with_cookie(target: &str, cookie: HeaderValue) -> Response {
    let mut resp = Response::builder()
        .status(StatusCode::SEE_OTHER)
        .body(axum::body::Body::empty())
        .expect("303 with empty body always builds");
    let headers = resp.headers_mut();
    headers.insert(
        LOCATION,
        HeaderValue::from_str(target).unwrap_or_else(|_| HeaderValue::from_static("/")),
    );
    headers.insert(SET_COOKIE, cookie);
    resp
}

fn urlenc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
