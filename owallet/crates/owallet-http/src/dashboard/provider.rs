//! Dashboard controls for OpenAI-compatible provider API keys.

use askama::Template;
use axum::extract::{Form, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use serde::Deserialize;

use super::{current_session, redirect_to_login};
use crate::error::AppError;
use crate::session::SessionRole;
use crate::state::AppState;
use crate::templates::ProviderKeyCreatedTemplate;

#[derive(Debug, Deserialize)]
pub struct RevokeProviderKeyForm {
    pub id: String,
}

#[derive(Debug, Deserialize, Default)]
pub struct CreateProviderKeyForm {
    /// HTML checkbox: present ("on") when the user opted the key into the
    /// wallet spending tools; absent otherwise. Chat-only is the default —
    /// spending power is never implied.
    #[serde(default)]
    pub allow_spend: Option<String>,
    /// Optional daily spending budget in USD (per day, wallet timezone).
    /// Blank means no limit. Applies to every key — chat turns are paid
    /// orders, so chat-only keys spend too.
    #[serde(default)]
    pub budget_usd: Option<String>,
}

/// Parse a user-typed budget field: blank/whitespace → no limit; otherwise
/// a positive dollar amount (up to cents precision) → cents.
pub(crate) fn parse_budget_usd(input: Option<&str>) -> Result<Option<i64>, &'static str> {
    let Some(raw) = input.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let raw = raw.strip_prefix('$').unwrap_or(raw);
    let usd: f64 = raw
        .parse()
        .map_err(|_| "budget must be a dollar amount, or blank for no limit")?;
    if !usd.is_finite() || usd <= 0.0 {
        return Err("budget must be a positive dollar amount, or blank for no limit");
    }
    let cents = (usd * 100.0).round() as i64;
    if cents <= 0 {
        return Err("budget must be at least $0.01, or blank for no limit");
    }
    Ok(Some(cents))
}

pub async fn create_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<CreateProviderKeyForm>,
) -> Result<Response, AppError> {
    let Some(session) = current_session(&state.sessions, &headers) else {
        return Ok(redirect_to_login().into_response());
    };
    let can_spend = form.allow_spend.is_some();
    let scopes = if can_spend { "chat spend" } else { "chat" };
    // The budget bounds everything the key costs — chat turns included —
    // so it applies to chat-only keys too.
    let budget_usd_cents = match parse_budget_usd(form.budget_usd.as_deref()) {
        Ok(v) => v,
        Err(_) => {
            return Ok(Redirect::to("/wallet?notice=provider-key-budget-invalid").into_response())
        }
    };
    let (npub, key) = {
        let db = state
            .db
            .lock()
            .map_err(|e| AppError::Internal(format!("db mutex poisoned: {e}")))?;
        let npub = match session.role {
            SessionRole::Wallet { npub } => npub,
            SessionRole::Admin => match db.read_default_npub()? {
                Some(npub) => npub,
                None => return Ok(Redirect::to("/wallet?notice=no-wallet").into_response()),
            },
        };
        let (_, key) = db.create_provider_key(&npub, "dashboard", scopes, budget_usd_cents)?;
        (npub, key)
    };
    Ok(Html(
        ProviderKeyCreatedTemplate {
            npub,
            key,
            can_spend,
            budget: budget_usd_cents.map(format_usd_cents),
        }
        .render()?,
    )
    .into_response())
}

#[derive(Debug, Deserialize)]
pub struct UpdateBudgetForm {
    pub id: String,
    /// New daily budget in USD — blank clears the limit. Today's spend is
    /// never reset by an edit: raising the budget takes effect
    /// immediately, and the window itself resets at the wallet-local midnight.
    #[serde(default)]
    pub budget_usd: Option<String>,
}

pub async fn update_budget_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<UpdateBudgetForm>,
) -> Result<Response, AppError> {
    let Some(session) = current_session(&state.sessions, &headers) else {
        return Ok(redirect_to_login().into_response());
    };
    let budget_usd_cents = match parse_budget_usd(form.budget_usd.as_deref()) {
        Ok(v) => v,
        Err(_) => {
            return Ok(Redirect::to("/wallet?notice=provider-key-budget-invalid").into_response())
        }
    };
    let db = state
        .db
        .lock()
        .map_err(|e| AppError::Internal(format!("db mutex poisoned: {e}")))?;
    let npub = match session.role {
        SessionRole::Wallet { npub } => npub,
        SessionRole::Admin => match db.read_default_npub()? {
            Some(npub) => npub,
            None => return Ok(Redirect::to("/wallet?notice=no-wallet").into_response()),
        },
    };
    db.update_provider_key_budget(&form.id, &npub, budget_usd_cents)?;
    Ok(Redirect::to("/wallet?notice=provider-key-budget-updated").into_response())
}

pub async fn revoke_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<RevokeProviderKeyForm>,
) -> Result<Response, AppError> {
    let Some(session) = current_session(&state.sessions, &headers) else {
        return Ok(redirect_to_login().into_response());
    };
    let db = state
        .db
        .lock()
        .map_err(|e| AppError::Internal(format!("db mutex poisoned: {e}")))?;
    let npub = match session.role {
        SessionRole::Wallet { npub } => npub,
        SessionRole::Admin => match db.read_default_npub()? {
            Some(npub) => npub,
            None => return Ok(Redirect::to("/wallet?notice=no-wallet").into_response()),
        },
    };
    db.delete_provider_key(&form.id, &npub)?;
    Ok(Redirect::to("/wallet?notice=provider-key-revoked").into_response())
}

/// `1234` cents → `"$12.34"`.
pub(crate) fn format_usd_cents(cents: i64) -> String {
    format!("${}.{:02}", cents / 100, (cents % 100).abs())
}
