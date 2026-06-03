//! Shared helpers for the Zcash (Orchard) commands: resolve network /
//! lightwalletd / per-wallet data directory from the environment, mirroring
//! how the EVM commands read `EVM_RPC_URL` / `EVM_NETWORK`.

use std::path::PathBuf;

use super::{CmdError, Result};

/// `ZEC_NETWORK` (default mainnet).
pub(crate) fn network() -> Result<owallet_zcash::Network> {
    let name = std::env::var("ZEC_NETWORK").unwrap_or_else(|_| "mainnet".into());
    owallet_zcash::Network::parse(&name).map_err(|e| CmdError::BadInput(e.to_string()))
}

/// `ZEC_LIGHTWALLETD_URL` (operator alias or host:port; default `zecrocks`).
pub(crate) fn lightwalletd() -> String {
    std::env::var("ZEC_LIGHTWALLETD_URL").unwrap_or_else(|_| "zecrocks".into())
}

/// Per-wallet Zcash data directory: the wallet's `<data dir>/<npub>/zcash/`
/// (honoring `OWALLET_HOME` / `ZEC_DATA_DIR`).
pub(crate) fn data_dir(npub: &str) -> Result<PathBuf> {
    owallet_zcash::data_dir_for(npub).map_err(|e| CmdError::BadInput(e.to_string()))
}
