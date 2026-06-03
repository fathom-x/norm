//! On-disk layout for the librustzcash wallet data, kept inside the wallet's
//! per-`npub` state directory (the #310 scheme) so a single backup of the
//! owallet data dir captures everything.
//!
//! Layout: `<data dir>/<npub>/zcash/` — where `<data dir>` is `OWALLET_HOME`
//! or `~/.owallet` (resolved by `owallet_db::wallet_state_dir`). Overridable
//! with `ZEC_DATA_DIR` (then `<ZEC_DATA_DIR>/<npub>/`). Each directory holds:
//!
//! - `data.sqlite`     — `zcash_client_sqlite::WalletDb`
//! - `blockmeta.sqlite` + `blocks/` — `FsBlockDb` compact-block cache
//!   (plaintext; public chain data)
//!
//! Proving parameters are bundled into the binary, so nothing is downloaded.

use std::path::{Path, PathBuf};

use crate::error::ZcashError;

pub(crate) const DATA_DB: &str = "data.sqlite";
pub(crate) const BLOCKS_FOLDER: &str = "blocks";
/// Subdirectory of the per-wallet state dir that holds the librustzcash files.
const ZCASH_SUBDIR: &str = "zcash";

/// Resolve (and create, `0700` on unix) the per-wallet Zcash data directory.
///
/// `<data dir>/<npub>/zcash/`, sharing the wallet's #310 per-`npub` state
/// directory (`owallet_db::wallet_state_dir`). `ZEC_DATA_DIR`, if set,
/// overrides the base — then `<ZEC_DATA_DIR>/<npub>/`. The `npub` is validated
/// so it can't escape the base via path traversal.
pub fn data_dir_for(npub: &str) -> Result<PathBuf, ZcashError> {
    let dir = match std::env::var("ZEC_DATA_DIR") {
        Ok(p) if !p.trim().is_empty() => PathBuf::from(p).join(sanitize_npub(npub)?),
        _ => owallet_db::wallet_state_dir(npub)
            .map_err(|e| ZcashError::Backend(format!("resolve wallet state dir: {e}")))?
            .join(ZCASH_SUBDIR),
    };
    std::fs::create_dir_all(&dir)?;
    set_dir_private(&dir);
    Ok(dir)
}

pub(crate) fn data_db_path(dir: &Path) -> PathBuf {
    dir.join(DATA_DB)
}

pub(crate) fn blocks_dir(dir: &Path) -> PathBuf {
    dir.join(BLOCKS_FOLDER)
}

/// Validate an npub so it's a safe single path component: ASCII alphanumeric
/// (bech32 npubs are `npub1…`, lowercase alnum), 1-128 chars, no separators or
/// traversal. (Only used for the `ZEC_DATA_DIR` override; the default path goes
/// through `owallet_db`'s own component validation.)
fn sanitize_npub(npub: &str) -> Result<&str, ZcashError> {
    let ok =
        !npub.is_empty() && npub.len() <= 128 && npub.chars().all(|c| c.is_ascii_alphanumeric());
    if ok {
        Ok(npub)
    } else {
        Err(ZcashError::Backend(format!(
            "invalid npub for data directory: {npub:?}"
        )))
    }
}

#[cfg(unix)]
fn set_dir_private(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(p) {
        let mut perms = meta.permissions();
        perms.set_mode(0o700);
        let _ = std::fs::set_permissions(p, perms);
    }
}

#[cfg(not(unix))]
fn set_dir_private(_p: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    // `data_dir_for` reads process-wide env (`ZEC_DATA_DIR`, and via
    // `owallet_db`, `OWALLET_HOME`), so the cases must not run concurrently.
    #[test]
    fn data_dir_resolution() {
        let tmp = tempfile::tempdir().unwrap();
        std::env::remove_var("ZEC_DATA_DIR");
        std::env::set_var("OWALLET_HOME", tmp.path());

        // Default: <OWALLET_HOME>/<npub>/zcash, created 0700.
        let dir = data_dir_for("npub1aaaa").unwrap();
        assert_eq!(dir, tmp.path().join("npub1aaaa").join("zcash"));
        assert!(dir.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }

        // npub traversal / separators rejected (via owallet_db validation).
        assert!(data_dir_for("../escape").is_err());
        assert!(data_dir_for("a/b").is_err());

        // ZEC_DATA_DIR override wins: <ZEC_DATA_DIR>/<npub>.
        let base = tmp.path().join("custom-base");
        std::env::set_var("ZEC_DATA_DIR", &base);
        let dir = data_dir_for("npub1bbbb").unwrap();
        assert_eq!(dir, base.join("npub1bbbb"));
        assert!(dir.is_dir());
        // Override still validates the npub component.
        assert!(data_dir_for("../escape").is_err());

        std::env::remove_var("ZEC_DATA_DIR");
        std::env::remove_var("OWALLET_HOME");
    }
}
