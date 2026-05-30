//! `POST /wallet/delete` — remove a wallet (admin-only).

use axum::extract::{Form, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;

use super::{current_session, redirect_to_login};
use crate::error::AppError;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct DeleteForm {
    pub npub: String,
}

pub async fn delete_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<DeleteForm>,
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

    db.delete_wallet(&form.npub)?;
    // If we just removed the default, clear it.
    if db.read_default_npub()?.as_deref() == Some(form.npub.as_str()) {
        // No explicit "unset" — overwrite with the first remaining wallet,
        // or do nothing if none remain. The DB layer doesn't expose a
        // delete-setting API; the next select call will overwrite anyway.
        if let Some(first) = db.list_wallets()?.first() {
            db.write_default_npub(&first.npub)?;
        }
    }

    Ok(Redirect::to("/wallet").into_response())
}
