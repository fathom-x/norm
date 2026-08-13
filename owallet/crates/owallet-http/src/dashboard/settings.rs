//! Wallet-wide preference handlers (`/wallet/settings/*`).

use axum::extract::{Form, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;

use super::{current_session, redirect_to_login};
use crate::error::AppError;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct TimezoneForm {
    /// IANA name ("Europe/Berlin"). Blank resets to UTC. Governs the
    /// daily-budget window boundary (and nothing else, so far — timestamp
    /// display stays UTC).
    #[serde(default)]
    pub timezone: String,
}

#[derive(Debug, Deserialize)]
pub struct SpendCapForm {
    /// Per-request spending cap in USD for the /v1 wallet tools. Blank
    /// clears the wallet-level override (reverting to the server's env
    /// override or the built-in $20 default).
    #[serde(default)]
    pub spend_cap_usd: Option<String>,
}

pub async fn spend_cap_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<SpendCapForm>,
) -> Result<Response, AppError> {
    if current_session(&state.sessions, &headers).is_none() {
        return Ok(redirect_to_login().into_response());
    }
    let cents = match super::provider::parse_budget_usd(form.spend_cap_usd.as_deref()) {
        Ok(v) => v,
        Err(_) => return Ok(Redirect::to("/wallet?notice=spend-cap-invalid").into_response()),
    };
    state
        .db
        .lock()
        .map_err(|e| AppError::Internal(format!("db mutex poisoned: {e}")))?
        .write_spend_cap_usd_cents(cents)?;
    Ok(Redirect::to("/wallet?notice=spend-cap-updated").into_response())
}

pub async fn timezone_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<TimezoneForm>,
) -> Result<Response, AppError> {
    if current_session(&state.sessions, &headers).is_none() {
        return Ok(redirect_to_login().into_response());
    }
    let name = form.timezone.trim();
    let name = if name.is_empty() { "UTC" } else { name };
    if !owallet_db::timezone_is_valid(name) {
        return Ok(Redirect::to("/wallet?notice=timezone-invalid").into_response());
    }
    state
        .db
        .lock()
        .map_err(|e| AppError::Internal(format!("db mutex poisoned: {e}")))?
        .write_timezone(name)?;
    Ok(Redirect::to("/wallet?notice=timezone-updated").into_response())
}
