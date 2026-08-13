//! `POST /wallet/send` — sign + broadcast a payment (USDC or ZEC).
//!
//! Mirrors the `owallet send` CLI command: look up the active wallet, and
//! depending on `asset` either derive the secp256k1 key and call
//! [`owallet_evm::send_usdc`], or derive the BIP-39 seed and call
//! [`owallet_zcash::send_zcash`] (Orchard). Renders a result page either way.
//! The form is plain HTML so the no-JS contract still holds.

use askama::Template;
use axum::extract::{Form, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use owallet_crypto::{bip39_seed_from_stored, derive_from_stored_seed};
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
    /// `usdc` (default) or `zec`.
    #[serde(default)]
    pub asset: Option<String>,
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

    // Route on the selected asset.
    if state_asset_is_zec(form.asset.as_deref()) {
        return send_zec(&state, npub, &seed, to, amount_usd).await;
    }

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

fn state_asset_is_zec(asset: Option<&str>) -> bool {
    matches!(
        asset.map(|a| a.trim().to_ascii_lowercase()).as_deref(),
        Some("zec")
    )
}

/// Sync + broadcast a shielded Orchard payment. The librustzcash futures are
/// not `Send`, so they run on a blocking thread with their own current-thread
/// runtime; the axum handler only awaits the `Send` `JoinHandle`.
async fn send_zec(
    state: &AppState,
    npub: String,
    stored_seed: &str,
    to: String,
    amount_zec: f64,
) -> Result<Response, AppError> {
    let seed = match bip39_seed_from_stored(stored_seed) {
        Ok(s) => s,
        Err(e) => {
            return Ok(
                render_error(&format!("This wallet has no Zcash account: {e}")).into_response(),
            )
        }
    };
    let network = match owallet_zcash::Network::parse(&state.zcash.network) {
        Ok(n) => n,
        Err(e) => return Ok(render_error(&format!("Zcash network config: {e}")).into_response()),
    };
    let dir = match owallet_zcash::data_dir_for(&npub) {
        Ok(d) => d,
        Err(e) => return Ok(render_error(&format!("Zcash data dir: {e}")).into_response()),
    };
    let lwd = state.zcash.lightwalletd.clone();
    let to_for_task = to.clone();

    let result = tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(owallet_zcash::ZcashError::from)?;
        rt.block_on(async move {
            owallet_zcash::sync(&dir, network, &lwd).await?;
            owallet_zcash::send_zcash(&dir, network, &lwd, &seed, &to_for_task, amount_zec).await
        })
    })
    .await
    .map_err(|e| AppError::Internal(format!("zcash task: {e}")))?;

    let outcome = match result {
        Ok(o) => o,
        Err(e) => return Ok(render_error(&format!("ZEC send failed: {e}")).into_response()),
    };

    let explorer_url = match network {
        owallet_zcash::Network::Main => Some(format!(
            "https://mainnet.zcashexplorer.app/transactions/{}",
            outcome.txid
        )),
        owallet_zcash::Network::Test => Some(format!(
            "https://testnet.zcashexplorer.app/transactions/{}",
            outcome.txid
        )),
    };
    let tpl = SendResultTemplate {
        ok: true,
        npub,
        chain_name: format!("Zcash ({})", network.name()),
        to: outcome.to,
        amount: format!("{} ZEC", outcome.amount_human),
        tx_hash: outcome.txid,
        block_number: None,
        explorer_url,
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
