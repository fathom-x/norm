//! `GET|POST /wallet/import` — admin-only.

use askama::Template;
use axum::extract::{Form, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use owallet_crypto::{
    derive_from_mnemonic, npub_from_private_key, Address, Mnemonic, PrivateKey, EVM_HD_PATH,
};
use serde::Deserialize;

use super::{current_session, redirect_to_login};
use crate::error::AppError;
use crate::state::AppState;
use crate::templates::ImportTemplate;

pub async fn import_get(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let Some(session) = current_session(&state.sessions, &headers) else {
        return Ok(redirect_to_login().into_response());
    };
    if !session.is_admin() {
        return Ok(redirect_to_login().into_response());
    }
    let tpl = ImportTemplate { error: None };
    Ok(Html(tpl.render()?).into_response())
}

#[derive(Debug, Deserialize)]
pub struct ImportForm {
    pub material: String,
    #[serde(default)]
    pub wallet_password: String,
    #[serde(default)]
    pub confirm_password: String,
}

pub async fn import_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<ImportForm>,
) -> Result<Response, AppError> {
    let Some(session) = current_session(&state.sessions, &headers) else {
        return Ok(redirect_to_login().into_response());
    };
    if !session.is_admin() {
        return Ok(redirect_to_login().into_response());
    }

    let trimmed = form.material.trim();
    let result = if trimmed.split_whitespace().count() >= 12 {
        Mnemonic::parse(trimmed)
            .map_err(|e| e.to_string())
            .and_then(|m| {
                derive_from_mnemonic(&m, EVM_HD_PATH)
                    .map(|sk| (m.phrase(), sk))
                    .map_err(|e| e.to_string())
            })
    } else {
        PrivateKey::from_hex(trimmed)
            .map(|sk| (format!("0x{}", sk.to_hex()), sk))
            .map_err(|e| e.to_string())
    };

    let (seed, sk) = match result {
        Ok(v) => v,
        Err(msg) => {
            let tpl = ImportTemplate { error: Some(msg) };
            return Ok(Html(tpl.render()?).into_response());
        }
    };

    let address = Address::from_private_key(&sk);
    let npub = npub_from_private_key(&sk).map_err(|e| AppError::Internal(e.to_string()))?;

    let db = state
        .db
        .lock()
        .map_err(|e| AppError::Internal(format!("db mutex poisoned: {e}")))?;

    // A per-wallet password (used to log into the web admin) is required only
    // when the imported wallet doesn't already have one — matches
    // `_wallet_import_post` in wallet_mcp/server.py.
    let needs_password = !db.has_wallet_password(&npub)?;
    if needs_password {
        if form.wallet_password.is_empty() {
            let tpl = ImportTemplate {
                error: Some("Please choose a wallet password.".to_string()),
            };
            return Ok(Html(tpl.render()?).into_response());
        }
        if form.wallet_password != form.confirm_password {
            let tpl = ImportTemplate {
                error: Some("Passwords do not match.".to_string()),
            };
            return Ok(Html(tpl.render()?).into_response());
        }
    }

    db.write_wallet(&npub, &seed, Some(&address.to_hex_lower()))?;
    if db.read_default_npub()?.is_none() {
        db.write_default_npub(&npub)?;
    }
    if needs_password {
        db.write_wallet_password(&npub, &form.wallet_password)?;
    }
    drop(db);

    Ok(Redirect::to("/wallet").into_response())
}
