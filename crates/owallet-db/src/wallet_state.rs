//! Per-wallet state directory + encrypted artifact store.
//!
//! Implements the per-`npub` layout from issue #310: each wallet owns a
//! subdirectory under the owallet data directory, and every artifact written
//! into it (chain sync state, etc.) is encrypted at rest with a key derived
//! from *that wallet's* private key (see [`owallet_crypto::derive_state_key`]).
//! The guiding principle is that "backing up a fully synced wallet" is just
//! "copy the data directory" — keys, config, and chain state all live together.
//!
//! ## Layout
//!
//! ```text
//! <data dir>/                 # ~/.owallet (override: OWALLET_HOME)
//!   <npub>/                   # one subdir per wallet
//!     <artifact>              # nonce(16) || AES-256-GCM ciphertext||tag
//! ```
//!
//! ## On-disk artifact format
//!
//! `nonce (16 bytes) || ciphertext || tag (16 bytes)`. The nonce is random per
//! write; the AES key is the wallet's derived state key, so the bytes are
//! meaningless without the wallet's private key.

use std::path::{Path, PathBuf};

use owallet_crypto::{decrypt, encrypt, AesKey};

use crate::{DbError, Result};

/// Length of the random nonce prefix on each encrypted artifact.
const NONCE_LEN: usize = owallet_crypto::aesgcm::NONCE_LEN;

/// Base data directory for all wallet state: `OWALLET_HOME` if set, else
/// `~/.owallet`. This is a *directory* (distinct from the legacy single-file
/// DB at `~/.owallet.db`, which [`crate::default_db_path`] still returns).
#[must_use]
pub fn data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("OWALLET_HOME") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".owallet")
}

/// The per-wallet state directory for `npub`: `<data dir>/<npub>`.
///
/// `npub`s are bech32 (`npub1…`, lowercase alphanumeric) so they are safe,
/// stable directory names. As defence-in-depth the value is still validated
/// to be a single path component (no separators, no `..`).
pub fn wallet_state_dir(npub: &str) -> Result<PathBuf> {
    validate_component(npub)
        .map_err(|_| DbError::State(format!("invalid npub for state directory: {npub:?}")))?;
    Ok(data_dir().join(npub))
}

/// An open handle to one wallet's encrypted state directory.
///
/// Construct via [`crate::Database::wallet_state`] (which derives the key from
/// the stored seed) or [`WalletStateDir::new`] when you already hold the key.
pub struct WalletStateDir {
    dir: PathBuf,
    key: AesKey,
}

impl WalletStateDir {
    /// Build a handle for `dir` encrypting with `key`. The directory is created
    /// lazily on the first write.
    #[must_use]
    pub fn new(dir: PathBuf, key: AesKey) -> Self {
        Self { dir, key }
    }

    /// The wallet's state directory path (may not exist yet).
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.dir
    }

    /// Resolve a relative artifact name to an absolute path inside the wallet
    /// directory, rejecting anything that would escape it (`..`, absolute,
    /// path separators).
    fn resolve(&self, name: &str) -> Result<PathBuf> {
        validate_component(name)
            .map_err(|e| DbError::State(format!("invalid artifact name {name:?}: {e}")))?;
        Ok(self.dir.join(name))
    }

    /// Encrypt `plaintext` with the wallet key and write it to `name`,
    /// creating the directory if needed. The write is atomic (temp file +
    /// rename) so a crash mid-write can't leave a half-written artifact.
    pub fn write(&self, name: &str, plaintext: &[u8]) -> Result<()> {
        let path = self.resolve(name)?;
        std::fs::create_dir_all(&self.dir)?;

        let (ct, nonce) = encrypt(&self.key, plaintext);
        let mut blob = Vec::with_capacity(NONCE_LEN + ct.len());
        blob.extend_from_slice(&nonce);
        blob.extend_from_slice(&ct);

        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &blob)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Read and decrypt `name`. Returns `Ok(None)` if the artifact does not
    /// exist; errors if it is malformed or fails authentication (wrong wallet
    /// key / tampering).
    pub fn read(&self, name: &str) -> Result<Option<Vec<u8>>> {
        let path = self.resolve(name)?;
        let blob = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(DbError::Io(e)),
        };
        if blob.len() < NONCE_LEN {
            return Err(DbError::State(format!(
                "artifact {name:?} is truncated ({} bytes)",
                blob.len()
            )));
        }
        let (nonce, ct) = blob.split_at(NONCE_LEN);
        Ok(Some(decrypt(&self.key, ct, nonce)?))
    }

    /// Whether `name` exists in the wallet directory.
    #[must_use]
    pub fn exists(&self, name: &str) -> bool {
        match self.resolve(name) {
            Ok(p) => p.exists(),
            Err(_) => false,
        }
    }

    /// Delete `name`. Idempotent — a missing artifact is not an error.
    pub fn remove(&self, name: &str) -> Result<()> {
        let path = self.resolve(name)?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(DbError::Io(e)),
        }
    }
}

/// Reject path components that aren't a single safe segment.
fn validate_component(name: &str) -> std::result::Result<(), &'static str> {
    if name.is_empty() {
        return Err("empty");
    }
    if name == "." || name == ".." {
        return Err("dot segment");
    }
    if name.contains('/') || name.contains('\\') || name.contains('\0') {
        return Err("path separator");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> AesKey {
        AesKey::new([0x42u8; 32])
    }

    #[test]
    fn data_dir_honors_owallet_home() {
        // Note: env mutation is process-global; this test stands alone.
        std::env::set_var("OWALLET_HOME", "/tmp/owallet-test-home");
        assert_eq!(data_dir(), PathBuf::from("/tmp/owallet-test-home"));
        std::env::remove_var("OWALLET_HOME");
    }

    #[test]
    fn wallet_state_dir_rejects_traversal() {
        assert!(wallet_state_dir("../escape").is_err());
        assert!(wallet_state_dir("a/b").is_err());
        assert!(wallet_state_dir("npub1validlooking").is_ok());
    }

    #[test]
    fn write_then_read_roundtrips() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WalletStateDir::new(tmp.path().join("npub1abc"), key());
        store.write("sync.dat", b"zcash chain state").unwrap();
        assert_eq!(
            store.read("sync.dat").unwrap().unwrap(),
            b"zcash chain state"
        );
    }

    #[test]
    fn read_missing_is_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WalletStateDir::new(tmp.path().join("npub1abc"), key());
        assert!(store.read("absent").unwrap().is_none());
    }

    #[test]
    fn on_disk_bytes_are_encrypted() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("npub1abc");
        let store = WalletStateDir::new(dir.clone(), key());
        let secret = b"plaintext-marker-xyzzy";
        store.write("a", secret).unwrap();
        let raw = std::fs::read(dir.join("a")).unwrap();
        // The plaintext marker must not appear verbatim on disk.
        assert!(!raw.windows(secret.len()).any(|w| w == secret));
        // And there's a 16-byte nonce prefix ahead of the ciphertext+tag.
        assert_eq!(raw.len(), NONCE_LEN + secret.len() + 16);
    }

    #[test]
    fn wrong_key_fails_to_decrypt() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("npub1abc");
        WalletStateDir::new(dir.clone(), key())
            .write("a", b"secret")
            .unwrap();
        let other = WalletStateDir::new(dir, AesKey::new([0x99u8; 32]));
        assert!(other.read("a").is_err());
    }

    #[test]
    fn remove_is_idempotent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WalletStateDir::new(tmp.path().join("npub1abc"), key());
        store.write("a", b"x").unwrap();
        assert!(store.exists("a"));
        store.remove("a").unwrap();
        assert!(!store.exists("a"));
        store.remove("a").unwrap(); // second remove is a no-op
    }

    #[test]
    fn write_rejects_bad_names() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WalletStateDir::new(tmp.path().join("npub1abc"), key());
        assert!(store.write("../oops", b"x").is_err());
        assert!(store.write("a/b", b"x").is_err());
    }
}
