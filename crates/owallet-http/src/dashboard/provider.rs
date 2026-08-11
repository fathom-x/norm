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

pub async fn create_post(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let Some(session) = current_session(&state.sessions, &headers) else {
        return Ok(redirect_to_login().into_response());
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
        let (_, key) = db.create_provider_key(&npub, "dashboard")?;
        (npub, key)
    };
    Ok(Html(ProviderKeyCreatedTemplate { npub, key }.render()?).into_response())
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
