//! Orchard-only Zcash wallet backend for owallet, built on librustzcash.
//!
//! Mirrors the shape of `owallet-evm`: a small set of free functions the
//! CLI / MCP / HTTP layers call to receive, sync, show a balance, and send.
//! State lives in a per-wallet directory alongside the owallet DB (see
//! [`paths::data_dir_for`]); the privacy-sensitive wallet DB is encrypted at
//! rest with a seed-derived SQLCipher key (see `db`). Proving parameters are
//! bundled into the binary, so nothing is downloaded at runtime.
//!
//! Typical flow:
//! 1. At wallet creation, derive the receive address with
//!    [`orchard_ua_from_seed`] and store it.
//! 2. On first use, [`init_account`] provisions the wallet DB + birthday.
//! 3. [`sync`] scans the chain; [`zec_balance`] reports the balance.
//! 4. [`send_zcash`] builds, proves, signs, and broadcasts a payment.

mod amount;
mod balance;
mod data;
mod db;
mod error;
mod keys;
mod network;
mod paths;
mod remote;
mod send;
mod sync;

pub use amount::{format_zec, parse_zec_to_zat, COIN};
pub use balance::{zec_balance, ZecBalance};
pub use data::init_account;
pub use error::ZcashError;
pub use keys::{is_zcash_address, orchard_ua_from_seed};
pub use network::Network;
pub use paths::data_dir_for;
pub use send::{send_zcash, SendZcashOutcome};
pub use sync::sync;
