//! `POST /wallet/send` — sign + broadcast an ERC-20 USDC transfer.
//!
//! Mirrors the `owallet send` CLI command: look up the active wallet,
//! derive the secp256k1 key from the stored seed, resolve the chain via
//! the configured CAIP-2 network id, call [`owallet_evm::send_usdc`], and
//! render a result page (success or error). The form is plain HTML so
//! the no-JS contract still holds.

use askama::Template;
use axum::extract::{Form, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use owallet_crypto::derive_from_stored_seed;
use owallet_evm::chains;
use serde::Deserialize;

use super::{current_session, redirect_to_login};
use crate::error::AppError;
use crate::session::SessionRole;
use crate::state::AppState;
use crate::templates::SendResultTemplate;

#[derive(Debug, Deserialize)]
pub struct SendForm {
    pub to: String,
    pub amount: String,
}

pub async fn send_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<SendForm>,
) -> Result<Response, AppError> {
    let Some(session) = current_session(&state.sessions, &headers) else {
        return Ok(redirect_to_login().into_response());
    };

    // Validate + parse the amount before touching any DB / RPC state.
    let amount_usd: f64 = match form.amount.trim().parse() {
        Ok(a) if a > 0.0 && f64::is_finite(a) => a,
        _ => {
            return Ok(render_error("Amount must be a positive number.").into_response());
        }
    };
    let to = form.to.trim().to_string();
    if to.is_empty() {
        return Ok(render_error("Recipient address is required.").into_response());
    }

    // Resolve which wallet to send from: the bound npub for a wallet
    // session, or the default for an admin session.
    let (npub, seed) = {
        let db = state
            .db
            .lock()
            .map_err(|e| AppError::Internal(format!("db mutex poisoned: {e}")))?;
        let npub = match &session.role {
            SessionRole::Wallet { npub } => npub.clone(),
            SessionRole::Admin => match db.read_default_npub()? {
                Some(n) => n,
                None => {
                    return Ok(render_error("No default wallet selected.").into_response());
                }
            },
        };
        let seed = match db.read_seed(&npub)? {
            Some(s) => s,
            None => {
                return Ok(render_error(&format!(
                    "No seed stored for wallet {npub} — db is locked or wallet was deleted."
                ))
                .into_response());
            }
        };
        (npub, seed)
    };

    let sk = derive_from_stored_seed(&seed).map_err(|e| AppError::Internal(e.to_string()))?;
    let chain = match chains::from_caip2(&state.evm.network) {
        Ok(c) => c,
        Err(e) => return Ok(render_error(&format!("Chain config error: {e}")).into_response()),
    };

    let outcome =
        match owallet_evm::send_usdc(&state.evm.rpc_url, &chain, &sk, &to, amount_usd).await {
            Ok(o) => o,
            Err(e) => return Ok(render_error(&format!("Send failed: {e}")).into_response()),
        };

    let tpl = SendResultTemplate {
        ok: true,
        npub,
        chain_name: chain.name.to_string(),
        to: outcome.to,
        amount: outcome.amount_human,
        tx_hash: outcome.tx_hash,
        block_number: outcome.block_number,
        explorer_url: outcome.explorer_url,
        error: None,
    };
    Ok(Html(tpl.render()?).into_response())
}

fn render_error(msg: &str) -> Response {
    let tpl = SendResultTemplate {
        ok: false,
        npub: String::new(),
        chain_name: String::new(),
        to: String::new(),
        amount: String::new(),
        tx_hash: String::new(),
        block_number: None,
        explorer_url: None,
        error: Some(msg.to_string()),
    };
    match tpl.render() {
        Ok(body) => (StatusCode::BAD_REQUEST, Html(body)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Html(format!("<pre>render error: {e}</pre>")),
        )
            .into_response(),
    }
}
