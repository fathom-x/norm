//! `GET|POST /wallet/generate` — generate a new BIP-39 wallet (admin-only).

use askama::Template;
use axum::extract::{Form, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Response};
use owallet_crypto::{
    derive_from_mnemonic, npub_from_private_key, Address, Mnemonic, WordCount, EVM_HD_PATH,
};
use serde::Deserialize;

use super::{current_session, redirect_to_login};
use crate::error::AppError;
use crate::state::AppState;
use crate::templates::{GenerateSeedTemplate, GenerateTemplate};

pub async fn generate_get(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let Some(session) = current_session(&state.sessions, &headers) else {
        return Ok(redirect_to_login().into_response());
    };
    if !session.is_admin() {
        return Ok(super::redirect_to_login().into_response());
    }
    let tpl = GenerateTemplate {
        error: None,
        words: 24,
    };
    Ok(Html(tpl.render()?).into_response())
}

#[derive(Debug, Deserialize)]
pub struct GenerateForm {
    pub words: u8,
    #[serde(default)]
    pub wallet_password: String,
    #[serde(default)]
    pub confirm_password: String,
}

pub async fn generate_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<GenerateForm>,
) -> Result<Response, AppError> {
    let Some(session) = current_session(&state.sessions, &headers) else {
        return Ok(redirect_to_login().into_response());
    };
    if !session.is_admin() {
        return Ok(super::redirect_to_login().into_response());
    }

    let count = match form.words {
        12 => WordCount::Twelve,
        24 => WordCount::TwentyFour,
        n => {
            let tpl = GenerateTemplate {
                error: Some(format!("Mnemonic length must be 12 or 24, got {n}.")),
                words: 24,
            };
            return Ok(Html(tpl.render()?).into_response());
        }
    };

    // Per-wallet password (used to log into the web admin) is required —
    // matches `_wallet_generate_post` in wallet_mcp/server.py.
    let render_err = |msg: &str| -> Result<Response, AppError> {
        let tpl = GenerateTemplate {
            error: Some(msg.to_string()),
            words: form.words,
        };
        Ok(Html(tpl.render()?).into_response())
    };
    if form.wallet_password.is_empty() {
        return render_err("Please choose a wallet password.");
    }
    if form.wallet_password != form.confirm_password {
        return render_err("Passwords do not match.");
    }

    let mnemonic = Mnemonic::generate(count);
    let phrase = mnemonic.phrase();
    let sk = derive_from_mnemonic(&mnemonic, EVM_HD_PATH)
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let address = Address::from_private_key(&sk).to_checksum();
    let npub = npub_from_private_key(&sk).map_err(|e| AppError::Internal(e.to_string()))?;

    let db = state
        .db
        .lock()
        .map_err(|e| AppError::Internal(format!("db mutex poisoned: {e}")))?;
    db.write_wallet(&npub, &phrase, Some(&address.to_lowercase()))?;
    if db.read_default_npub()?.is_none() {
        db.write_default_npub(&npub)?;
    }
    db.write_wallet_password(&npub, &form.wallet_password)?;
    drop(db);

    let tpl = GenerateSeedTemplate {
        npub,
        address,
        phrase,
    };
    Ok(Html(tpl.render()?).into_response())
}
