//! `GET /` and `GET /wallet`.

use askama::Template;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use owallet_overpay::Auth;
use serde::Deserialize;

use super::{current_session, redirect_to_login};
use crate::error::AppError;
use crate::session::SessionRole;
use crate::state::AppState;
use crate::templates::DashboardTemplate;

pub async fn redirect_to_wallet() -> Redirect {
    Redirect::permanent("/wallet")
}

#[derive(Debug, Deserialize, Default)]
pub struct DashboardQuery {
    #[serde(default)]
    pub notice: Option<String>,
    #[serde(default)]
    pub msg: Option<String>,
}

pub async fn dashboard(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<DashboardQuery>,
) -> Result<Response, AppError> {
    let Some(session) = current_session(&state.sessions, &headers) else {
        return Ok(redirect_to_login().into_response());
    };

    let (wallets, default_npub, stored_token) = {
        let db = state
            .db
            .lock()
            .map_err(|e| AppError::Internal(format!("db mutex poisoned: {e}")))?;

        let wallets = db.list_wallets()?;
        let default_npub = db.read_default_npub()?;

        // Determine the active wallet, then pull the stored bearer for it
        // under the dashboard's host_key (used both as the linked-flag and
        // for the live Overpay fetch below).
        let active_npub = match &session.role {
            SessionRole::Wallet { npub } => Some(npub.clone()),
            SessionRole::Admin => default_npub.clone(),
        };
        let stored_token = match active_npub.as_deref() {
            Some(n) => db.read_token(n, &state.host_key)?,
            None => None,
        };

        (wallets, default_npub, stored_token)
    };

    let active = match &session.role {
        SessionRole::Wallet { npub } => wallets.iter().find(|w| &w.npub == npub).cloned(),
        SessionRole::Admin => default_npub
            .as_deref()
            .and_then(|n| wallets.iter().find(|w| w.npub == n).cloned()),
    };
    let has_overpay_token = stored_token.is_some();

    // Best-effort live Overpay fetch: refresh the username + account
    // number for the linked wallet. Most Mullvad-style accounts have no
    // username, so the account number is what we surface.
    let overpay_live = match (stored_token.as_deref(), active.as_ref()) {
        (Some(token), Some(_)) => fetch_overpay_summary(&state, token, active.as_ref()).await,
        _ => OverpaySummary::default(),
    };

    // Best-effort on-chain balances. RPC failure surfaces in the
    // displayed string instead of blocking the page render.
    let balances = match active.as_ref().and_then(|w| w.address.as_deref()) {
        Some(addr) => fetch_balance_strings(&state.evm, addr).await,
        None => BalanceStrings::default(),
    };

    let notice = q.notice.as_deref().map(|n| match (n, q.msg.as_deref()) {
        ("authorized", _) => "Linked to Overpay.".to_string(),
        ("run-authorize", _) => {
            "No Overpay account linked yet — click Link Overpay account to authorize.".to_string()
        }
        ("authorize-error", Some(m)) => format!("Overpay link failed: {m}"),
        ("authorize-error", None) => "Overpay link failed.".to_string(),
        ("no-wallet", _) => "No default wallet selected.".to_string(),
        (other, _) => other.to_string(),
    });

    let tpl = DashboardTemplate {
        is_admin: session.is_admin(),
        active,
        wallets: if session.is_admin() {
            wallets
        } else {
            Vec::new()
        },
        default_npub,
        notice,
        has_overpay_token,
        overpay_username: overpay_live.username,
        overpay_account_number: overpay_live.account_number,
        chain_name: balances.chain_name,
        eth_balance: balances.eth,
        usdc_balance: balances.usdc,
    };
    Ok(Html(tpl.render()?).into_response())
}

#[derive(Default)]
struct OverpaySummary {
    username: Option<String>,
    account_number: Option<String>,
}

async fn fetch_overpay_summary(
    state: &AppState,
    token: &str,
    active: Option<&owallet_db::WalletRow>,
) -> OverpaySummary {
    let Ok(info) = state.overpay.account(Auth::Bearer(token)).await else {
        // Fall back to whatever we previously cached on the wallet row.
        return OverpaySummary {
            username: active.and_then(|w| w.overpay_username.clone()),
            account_number: None,
        };
    };
    // Cache the username opportunistically — same write path the CLI /
    // OAuth callback use.
    if let Some(u) = info.username.as_deref() {
        if let (Ok(db), Some(npub)) = (state.db.lock(), active.map(|w| &w.npub)) {
            let _ = db.cache_wallet_username(npub, u);
        }
    }
    OverpaySummary {
        username: info.username,
        account_number: info.account_number,
    }
}

#[derive(Default)]
struct BalanceStrings {
    chain_name: Option<String>,
    eth: Option<String>,
    usdc: Option<String>,
}

async fn fetch_balance_strings(evm: &crate::EvmConfig, address: &str) -> BalanceStrings {
    let chain = match owallet_evm::chains::from_caip2(&evm.network) {
        Ok(c) => c,
        Err(_) => return BalanceStrings::default(),
    };
    let eth = match owallet_evm::eth_balance(&evm.rpc_url, address).await {
        Ok(v) => format!("{} ETH", owallet_evm::format_amount(v, 18)),
        Err(e) => format!("(could not fetch: {e})"),
    };
    let usdc = match owallet_evm::usdc_balance(&evm.rpc_url, &chain, address).await {
        Ok(v) => format!(
            "{} USDC",
            owallet_evm::format_amount(v, chain.usdc_decimals)
        ),
        Err(e) => format!("(could not fetch: {e})"),
    };
    BalanceStrings {
        chain_name: Some(chain.name.to_string()),
        eth: Some(eth),
        usdc: Some(usdc),
    }
}
