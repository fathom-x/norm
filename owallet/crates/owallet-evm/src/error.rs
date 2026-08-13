//! Error type for the EVM crate.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum EvmError {
    #[error("invalid recipient address: {0}")]
    InvalidAddress(String),
    #[error("invalid private key")]
    InvalidKey,
    #[error("unsupported chain: {0}")]
    UnsupportedChain(String),
    #[error("amount overflows u256")]
    AmountOverflow,
    #[error("amount must be > 0")]
    NonPositiveAmount,
    #[error("rpc transport: {0}")]
    Transport(String),
    #[error("tx revert / pending: {0}")]
    TxFailed(String),
}
