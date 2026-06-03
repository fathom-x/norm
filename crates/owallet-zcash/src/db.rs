//! Opening the librustzcash wallet DB (`data.sqlite`).
//!
//! The wallet DB is stored unencrypted in the per-wallet data directory
//! (`0700`), matching the zecrocks/zkv reference wallet. It holds the account's
//! Unified *Full Viewing* Key and decrypted note metadata — privacy-sensitive,
//! but **not** spend-capable: the spending key is derived on demand from the
//! BIP-39 seed, which lives only in owallet's separately-encrypted DB.

use std::path::Path;

use rand::rngs::OsRng;
use rusqlite::{Connection, OpenFlags};
use zcash_client_sqlite::{util::SystemClock, WalletDb};
use zcash_protocol::consensus;

use crate::{error::ZcashError, network::Network, paths::data_db_path};

/// Concrete `WalletDb` type used throughout the crate.
pub(crate) type ZWalletDb = WalletDb<Connection, consensus::Network, SystemClock, OsRng>;

/// Open the wallet DB read/write, creating the file if absent.
pub(crate) fn open_wallet_db(dir: &Path, network: Network) -> Result<ZWalletDb, ZcashError> {
    WalletDb::for_path(data_db_path(dir), network.into(), SystemClock, OsRng)
        .map_err(ZcashError::backend)
}

/// Open a read-only `rusqlite::Connection` to `data.sqlite` for the handful of
/// direct SQL queries the sync path makes (rewind-target lookup).
pub(crate) fn open_raw_readonly(dir: &Path) -> Result<Connection, ZcashError> {
    Connection::open_with_flags(
        data_db_path(dir),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(ZcashError::backend)
}
