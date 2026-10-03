//! Where the bytes live. The native build keeps today's layout — the
//! database file plus plain files under the per-wallet state dir. The
//! browser build (`wasm32-unknown-unknown`) has no filesystem: the SQLite
//! database is the only store (its VFS, OPFS or memory, is the host's
//! choice), so everything else moves into tables beside the wallet data
//! and directory/permission housekeeping is skipped.

use std::path::Path;

use rusqlite::Connection;

/// True when compiled for the browser.
pub(crate) const IS_BROWSER: bool = cfg!(all(target_family = "wasm", target_os = "unknown"));

/// Does a database exist at `path`?
pub(crate) fn db_exists(path: &Path) -> bool {
    if IS_BROWSER {
        // No `stat` in the browser: ask the VFS by opening without
        // SQLITE_OPEN_CREATE, which fails for a missing file.
        Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).is_ok()
    } else {
        path.exists()
    }
}

/// `PRAGMA journal_mode=WAL`. The browser VFSes have no shared memory, so
/// there the pragma is best effort (SQLite keeps the rollback journal).
pub(crate) fn enable_wal(conn: &Connection) -> rusqlite::Result<()> {
    let res = conn.pragma_update(None, "journal_mode", "WAL");
    if IS_BROWSER {
        Ok(())
    } else {
        res
    }
}

/// Where the per-wallet order cache lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OrderCacheStore {
    /// One JSON file per order under `<data_dir>/<npub>/orders/` (native).
    Files,
    /// A table in the wallet database itself (the browser build).
    Table,
}

impl OrderCacheStore {
    pub(crate) fn for_target() -> Self {
        if IS_BROWSER {
            Self::Table
        } else {
            Self::Files
        }
    }
}
