//! EVM transaction signing + ERC-20 USDC transfers for owallet.
//!
//! Wraps the alloy stack so the rest of the workspace doesn't pull alloy
//! types across crate boundaries — alloy 0.x has churned across minor
//! versions, and isolating it here lets us bump it (or swap to ethers /
//! ethrs / etc.) without touching `owallet-mcp` or the binary.
//!
//! Port of `_send_usdc_async` in `wallet_mcp/server.py:1802`.

pub mod chains;
pub mod error;
pub mod usdc;

pub use chains::ChainInfo;
pub use error::EvmError;
pub use usdc::{eth_balance, format_amount, send_usdc, usdc_balance, SendUsdcOutcome};
