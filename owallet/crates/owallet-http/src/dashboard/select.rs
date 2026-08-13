//! `POST /wallet/select` — set the default wallet (admin-only).

use axum::extract::{Form, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;

use super::{current_session, redirect_to_login};
use crate::error::AppError;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct SelectForm {
    pub npub: String,
}

pub async fn select_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<SelectForm>,
) -> Result<Response, AppError> {
    let Some(session) = current_session(&state.sessions, &headers) else {
        return Ok(redirect_to_login().into_response());
    };
    if !session.is_admin() {
        return Ok(redirect_to_login().into_response());
    }

    let db = state
        .db
        .lock()
        .map_err(|e| AppError::Internal(format!("db mutex poisoned: {e}")))?;

    // Sanity check — refuse to set a default that doesn't exist.
    if db.list_wallets()?.iter().all(|w| w.npub != form.npub) {
        return Err(AppError::BadInput(format!("unknown wallet {}", form.npub)));
    }

    db.write_default_npub(&form.npub)?;
    Ok(Redirect::to("/wallet").into_response())
}
