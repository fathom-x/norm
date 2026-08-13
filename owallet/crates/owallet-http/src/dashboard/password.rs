//! `GET|POST /wallet/password` — set/change the per-wallet password.

use askama::Template;
use axum::extract::{Form, Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use serde::Deserialize;

use super::{current_session, redirect_to_login};
use crate::error::AppError;
use crate::state::AppState;
use crate::templates::PasswordTemplate;

#[derive(Debug, Deserialize)]
pub struct PasswordQuery {
    pub npub: Option<String>,
}

pub async fn password_get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<PasswordQuery>,
) -> Result<Response, AppError> {
    let Some(session) = current_session(&state.sessions, &headers) else {
        return Ok(redirect_to_login().into_response());
    };
    let npub = match (q.npub, &session.role) {
        (Some(n), crate::session::SessionRole::Admin) => n,
        (None, crate::session::SessionRole::Wallet { npub }) => npub.clone(),
        (Some(_), crate::session::SessionRole::Wallet { npub }) => npub.clone(),
        (None, crate::session::SessionRole::Admin) => {
            // Admin must pick which wallet to set a password for. Fall back
            // to the default if there is one.
            let db = state
                .db
                .lock()
                .map_err(|e| AppError::Internal(format!("db mutex poisoned: {e}")))?;
            match db.read_default_npub()? {
                Some(n) => n,
                None => return Err(AppError::BadInput("no default wallet".into())),
            }
        }
    };

    let tpl = PasswordTemplate { npub, error: None };
    Ok(Html(tpl.render()?).into_response())
}

#[derive(Debug, Deserialize)]
pub struct PasswordForm {
    pub npub: String,
    pub password: String,
    pub confirm: String,
}

pub async fn password_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<PasswordForm>,
) -> Result<Response, AppError> {
    let Some(session) = current_session(&state.sessions, &headers) else {
        return Ok(redirect_to_login().into_response());
    };

    // Wallet sessions can only set their own password.
    if let crate::session::SessionRole::Wallet { npub } = &session.role {
        if npub != &form.npub {
            return Err(AppError::BadInput(
                "wallet sessions cannot set passwords for other wallets".into(),
            ));
        }
    }

    if form.password != form.confirm {
        let tpl = PasswordTemplate {
            npub: form.npub,
            error: Some("Confirmation did not match.".into()),
        };
        return Ok(Html(tpl.render()?).into_response());
    }
    if form.password.is_empty() {
        let tpl = PasswordTemplate {
            npub: form.npub,
            error: Some("Password cannot be empty.".into()),
        };
        return Ok(Html(tpl.render()?).into_response());
    }

    let db = state
        .db
        .lock()
        .map_err(|e| AppError::Internal(format!("db mutex poisoned: {e}")))?;
    db.write_wallet_password(&form.npub, &form.password)?;
    drop(db);

    Ok(Redirect::to("/wallet").into_response())
}
