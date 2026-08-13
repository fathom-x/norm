//! Shared helpers for the subcommands that talk to the Overpay Rails API.

use owallet_config::defaults;
use owallet_overpay::OverpayClient;

use super::Result;

/// Resolve `OVERPAY_RAILS_URL` + `OVERPAY_PUBLIC_URL` from the process env
/// (already populated from `.owallet` files by `cli::load_env_from_flags`).
pub(crate) fn client() -> Result<OverpayClient> {
    let rails = std::env::var("OVERPAY_RAILS_URL")
        .unwrap_or_else(|_| defaults::OVERPAY_RAILS_URL.to_string());
    let public = std::env::var("OVERPAY_PUBLIC_URL").ok();
    let c = OverpayClient::new(&rails)?;
    let c = match public {
        Some(p) if p != rails => c.with_public_url(&p)?,
        _ => c,
    };
    Ok(c)
}

/// Stable host key under which the OAuth bearer token is stored — the
/// Overpay API base URL, normalized. Matches the Python `_host_key(rails_url)`
/// shape from `server.py:804`, and is the same key `owallet serve` derives
/// from its Overpay client, so a wallet linked here is linked there too.
pub(crate) fn host_key() -> String {
    let rails = std::env::var("OVERPAY_RAILS_URL")
        .unwrap_or_else(|_| defaults::OVERPAY_RAILS_URL.to_string());
    owallet_overpay::host_key(&rails)
}

/// Build a single tokio runtime for sync CLI handlers that need to drive
/// async HTTP calls.
pub(crate) fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
        .block_on(f)
}
