//! Error type for the Zcash backend. Mirrors `owallet_evm::EvmError` in shape:
//! a flat enum of the failure modes the wallet surfaces to the CLI / MCP / HTTP
//! layers, with `Backend` wrapping the librustzcash machinery.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ZcashError {
    #[error("invalid Zcash address: {0}")]
    InvalidAddress(String),

    #[error("invalid network {0:?} (use \"mainnet\" or \"testnet\")")]
    InvalidNetwork(String),

    #[error("amount must be positive and within range")]
    NonPositiveAmount,

    #[error("amount overflow")]
    AmountOverflow,

    #[error("this wallet has no Zcash account (hex-key wallets are unsupported)")]
    NoAccount,

    #[error("insufficient funds: have {available} zat, need {required} zat")]
    InsufficientFunds { available: u64, required: u64 },

    #[error("lightwalletd transport: {0}")]
    Transport(String),

    #[error("broadcast rejected (code {code}): {reason}")]
    SendFailed { code: i32, reason: String },

    #[error("wallet data directory error: {0}")]
    Io(#[from] std::io::Error),

    #[error("wallet backend: {0}")]
    Backend(String),
}

impl ZcashError {
    pub(crate) fn backend(e: impl std::fmt::Display) -> Self {
        ZcashError::Backend(e.to_string())
    }
    pub(crate) fn transport(e: impl std::fmt::Display) -> Self {
        ZcashError::Transport(e.to_string())
    }
}
