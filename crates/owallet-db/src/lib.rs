//! Encrypted SQLite storage for owallet.
//!
//! Byte-compatible with the Python `wallet_mcp.db` module: same schema, same
//! migration list, same crypto parameters (PBKDF2-SHA256/600k + AES-256-GCM
//! with 16-byte nonces and 16-byte tags appended to ciphertext).

mod access_tokens;
mod auth_codes;
mod oauth_clients;
mod purchases;
mod schema;
mod settings;
mod tokens;
mod wallet_state;
mod wallets;

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use owallet_crypto::{
    decrypt, derive_key, encrypt, hashes_equal, verify_hash, AesKey, DecryptError,
};
use rusqlite::Connection;
use thiserror::Error;
use zeroize::Zeroize;

pub use access_tokens::AccessTokenRow;
pub use auth_codes::AuthCodeRow;
pub use oauth_clients::OAuthClientRow;
pub use purchases::PurchaseRow;
pub use tokens::TokenRow;
pub use wallet_state::{data_dir, wallet_state_dir, WalletStateDir};
pub use wallets::WalletRow;

/// Default DB path: `~/.owallet.db` (override via `OWALLET_DB_PATH`).
#[must_use]
pub fn default_db_path() -> PathBuf {
    if let Ok(path) = std::env::var("OWALLET_DB_PATH") {
        return PathBuf::from(path);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".owallet.db")
}

#[derive(Debug, Error)]
pub enum DbError {
    #[error("database not found at {0}; run `owallet init` first")]
    NotFound(PathBuf),
    #[error("database already exists at {0}")]
    AlreadyExists(PathBuf),
    #[error("database is locked; call unlock() first")]
    Locked,
    #[error("decryption failed: {0}")]
    Decrypt(#[from] DecryptError),
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("invalid stored data: {0}")]
    Corrupt(String),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("wallet state: {0}")]
    State(String),
    #[error("key derivation: {0}")]
    KeyDerivation(#[from] owallet_crypto::HdError),
}

pub type Result<T> = std::result::Result<T, DbError>;

/// Handle to the encrypted owallet database.
///
/// Created via [`Database::init`] (fresh DB) or [`Database::open`] + [`Database::unlock`]
/// (existing DB). The AES key is held in memory only and zeroized on drop.
pub struct Database {
    conn: Connection,
    key: Option<AesKey>,
    path: PathBuf,
}

impl Database {
    /// Create a new encrypted database at `path` with the given password.
    /// Fails if the file already exists.
    pub fn init(path: &Path, password: &str) -> Result<Self> {
        if path.exists() {
            return Err(DbError::AlreadyExists(path.to_path_buf()));
        }
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).ok();
            }
        }

        let conn = Connection::open(path)?;
        schema::create(&conn)?;

        let mut salt = [0u8; owallet_crypto::SALT_LEN];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut salt);

        settings::write(&conn, "db_salt", &hex::encode(salt))?;
        settings::write(&conn, "password_hash", &verify_hash(password, &salt))?;

        let mut db = Self {
            conn,
            key: None,
            path: path.to_path_buf(),
        };
        // Derive the AES key and leave the DB unlocked, matching db.py:189.
        db.key = Some(AesKey::new(derive_key(password, &salt)));
        // `salt` will be zeroized on scope exit by the Zeroize derive on its
        // wrapper... except we held it as a raw [u8;32]. Explicit wipe.
        let mut salt_to_wipe = salt;
        salt_to_wipe.zeroize();
        Ok(db)
    }

    /// Open an existing database without unlocking it. The connection runs
    /// schema migrations on first open (matching `db.py::_migrate`).
    pub fn open(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Err(DbError::NotFound(path.to_path_buf()));
        }
        let conn = Connection::open(path)?;
        // Apply the standard PRAGMAs and migrations.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        schema::migrate(&conn)?;
        Ok(Self {
            conn,
            key: None,
            path: path.to_path_buf(),
        })
    }

    /// Check whether a DB file exists at the given path.
    #[must_use]
    pub fn exists(path: &Path) -> bool {
        path.exists()
    }

    /// Verify the password matches and derive the AES key. Returns Ok(false)
    /// on wrong password (no state change), Ok(true) on success.
    pub fn unlock(&mut self, password: &str) -> Result<bool> {
        let Some(salt_hex) = settings::read(&self.conn, "db_salt")? else {
            return Ok(false);
        };
        let Some(stored_hash) = settings::read(&self.conn, "password_hash")? else {
            return Ok(false);
        };
        let salt = hex::decode(&salt_hex)
            .map_err(|e| DbError::Corrupt(format!("db_salt is not hex: {e}")))?;

        let check = verify_hash(password, &salt);
        if !hashes_equal(&check, &stored_hash) {
            return Ok(false);
        }

        self.key = Some(AesKey::new(derive_key(password, &salt)));
        Ok(true)
    }

    /// Verify a password without unlocking. Returns true on match.
    pub fn verify_password(&self, password: &str) -> Result<bool> {
        let Some(salt_hex) = settings::read(&self.conn, "db_salt")? else {
            return Ok(false);
        };
        let Some(stored_hash) = settings::read(&self.conn, "password_hash")? else {
            return Ok(false);
        };
        let salt = hex::decode(&salt_hex)
            .map_err(|e| DbError::Corrupt(format!("db_salt is not hex: {e}")))?;
        Ok(hashes_equal(&verify_hash(password, &salt), &stored_hash))
    }

    /// Drop the in-memory AES key. Subsequent operations that touch encrypted
    /// data will return [`DbError::Locked`].
    pub fn lock(&mut self) {
        self.key = None;
    }

    #[must_use]
    pub fn is_unlocked(&self) -> bool {
        self.key.is_some()
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn key(&self) -> Result<&AesKey> {
        self.key.as_ref().ok_or(DbError::Locked)
    }

    // ---- Wallets ----

    /// Store or replace an encrypted wallet seed.
    pub fn write_wallet(&self, npub: &str, seed: &str, address: Option<&str>) -> Result<()> {
        let key = self.key()?;
        let (ct, nonce) = encrypt(key, seed.as_bytes());
        wallets::insert(&self.conn, npub, &ct, &nonce, address, now_secs())?;
        Ok(())
    }

    /// Decrypt and return the seed for `npub`, or `None` if not stored.
    pub fn read_seed(&self, npub: &str) -> Result<Option<String>> {
        let Some((ct, nonce)) = wallets::read_blob(&self.conn, npub)? else {
            return Ok(None);
        };
        let pt = decrypt(self.key()?, &ct, &nonce)?;
        Ok(Some(String::from_utf8(pt).map_err(|e| {
            DbError::Corrupt(format!("seed is not valid UTF-8: {e}"))
        })?))
    }

    /// Delete a wallet by npub. Idempotent.
    pub fn delete_wallet(&self, npub: &str) -> Result<()> {
        wallets::delete(&self.conn, npub)
    }

    /// List wallets (does not decrypt).
    pub fn list_wallets(&self) -> Result<Vec<WalletRow>> {
        wallets::list(&self.conn)
    }

    /// Decrypt every wallet's seed. Errors if locked.
    pub fn read_all_seeds(&self) -> Result<Vec<(String, String)>> {
        let key = self.key()?;
        let blobs = wallets::list_blobs(&self.conn)?;
        let mut out = Vec::with_capacity(blobs.len());
        for (npub, ct, nonce) in blobs {
            let pt = decrypt(key, &ct, &nonce)?;
            let seed = String::from_utf8(pt)
                .map_err(|e| DbError::Corrupt(format!("seed not utf-8: {e}")))?;
            out.push((npub, seed));
        }
        Ok(out)
    }

    pub fn find_wallet_by_identifier(&self, identifier: &str) -> Result<Option<String>> {
        let id = identifier.trim();
        let id_lower = id.to_lowercase();
        for w in self.list_wallets()? {
            if w.npub == id {
                return Ok(Some(w.npub));
            }
            if w.address.as_deref().map(str::to_lowercase).as_deref() == Some(&id_lower) {
                return Ok(Some(w.npub));
            }
            if w.overpay_username
                .as_deref()
                .map(str::to_lowercase)
                .as_deref()
                == Some(&id_lower)
            {
                return Ok(Some(w.npub));
            }
        }
        Ok(None)
    }

    pub fn cache_wallet_address(&self, npub: &str, address: &str) -> Result<()> {
        wallets::set_address(&self.conn, npub, address)
    }

    pub fn cache_wallet_username(&self, npub: &str, username: &str) -> Result<()> {
        wallets::set_username(&self.conn, npub, username)
    }

    pub fn touch_wallet(&self, npub: &str) -> Result<()> {
        wallets::touch(&self.conn, npub, now_secs())
    }

    pub fn write_wallet_password(&self, npub: &str, password: &str) -> Result<()> {
        let mut salt = [0u8; owallet_crypto::SALT_LEN];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut salt);
        // Per-wallet hash uses the *600k* iteration count (db.py:343-350).
        let key = derive_key(password, &salt);
        let stored = format!("{}:{}", hex::encode(salt), hex::encode(key));
        wallets::set_password_hash(&self.conn, npub, &stored)?;
        Ok(())
    }

    pub fn verify_wallet_password(&self, npub: &str, password: &str) -> Result<bool> {
        let Some(stored) = wallets::read_password_hash(&self.conn, npub)? else {
            return Ok(false);
        };
        let Some((salt_hex, hash_hex)) = stored.split_once(':') else {
            return Ok(false);
        };
        let salt = hex::decode(salt_hex)
            .map_err(|e| DbError::Corrupt(format!("wallet salt not hex: {e}")))?;
        let derived = derive_key(password, &salt);
        Ok(hashes_equal(&hex::encode(derived), hash_hex))
    }

    pub fn has_wallet_password(&self, npub: &str) -> Result<bool> {
        Ok(wallets::read_password_hash(&self.conn, npub)?.is_some())
    }

    // ---- Per-wallet encrypted state directory (issue #310) ----

    /// Open the per-`npub` encrypted state directory (`<data dir>/<npub>/`).
    ///
    /// Requires the DB to be unlocked: the directory's encryption key is
    /// derived from the wallet's own private key, recovered from the stored
    /// (encrypted) seed — so artifacts under it are bound to the wallet, not
    /// the DB password. Errors if no wallet is stored for `npub`.
    pub fn wallet_state(&self, npub: &str) -> Result<WalletStateDir> {
        let seed = self
            .read_seed(npub)?
            .ok_or_else(|| DbError::State(format!("no stored wallet for {npub}")))?;
        let sk = owallet_crypto::derive_from_stored_seed(&seed)?;
        let key = owallet_crypto::derive_state_key(&sk);
        Ok(WalletStateDir::new(wallet_state_dir(npub)?, key))
    }

    // ---- Purchase cache ----

    /// Store/refresh a cached order for `npub`. `order` is the Rails order
    /// payload (already unwrapped from any `{data: …}` envelope). Returns the
    /// order_id, or `None` if the payload has no id. Does not require unlock —
    /// the cached fields are plaintext metadata, like the wallet address.
    pub fn upsert_purchase(&self, npub: &str, order: &serde_json::Value) -> Result<Option<String>> {
        purchases::upsert(&self.conn, npub, order, now_secs())
    }

    /// Cached purchases for `npub`, newest first.
    pub fn list_purchases(
        &self,
        npub: &str,
        limit: i64,
        offset: i64,
        fulfillment_status: Option<&str>,
    ) -> Result<Vec<PurchaseRow>> {
        purchases::list(&self.conn, npub, limit, offset, fulfillment_status)
    }

    /// A single cached purchase by `(npub, order_id)`.
    pub fn read_purchase(&self, npub: &str, order_id: &str) -> Result<Option<PurchaseRow>> {
        purchases::read(&self.conn, npub, order_id)
    }

    pub fn delete_purchase(&self, npub: &str, order_id: &str) -> Result<()> {
        purchases::delete(&self.conn, npub, order_id)
    }

    pub fn count_purchases(&self, npub: &str) -> Result<i64> {
        purchases::count(&self.conn, npub)
    }

    // ---- Default npub ----

    pub fn write_default_npub(&self, npub: &str) -> Result<()> {
        settings::write(&self.conn, "default_npub", npub)?;
        Ok(())
    }

    pub fn read_default_npub(&self) -> Result<Option<String>> {
        Ok(settings::read(&self.conn, "default_npub")?)
    }

    // ---- Tokens ----

    pub fn write_token(&self, npub: &str, host: &str, token: &str, token_name: &str) -> Result<()> {
        let key = self.key()?;
        let (ct, nonce) = encrypt(key, token.as_bytes());
        tokens::insert(&self.conn, npub, host, &ct, &nonce, token_name, now_secs())
    }

    pub fn read_token(&self, npub: &str, host: &str) -> Result<Option<String>> {
        let Some((ct, nonce)) = tokens::read_blob(&self.conn, npub, host)? else {
            return Ok(None);
        };
        let pt = decrypt(self.key()?, &ct, &nonce)?;
        Ok(Some(String::from_utf8(pt).map_err(|e| {
            DbError::Corrupt(format!("token not utf-8: {e}"))
        })?))
    }

    pub fn delete_token(&self, npub: &str, host: &str) -> Result<()> {
        tokens::delete(&self.conn, npub, host)
    }

    // ---- OAuth clients ----

    pub fn write_oauth_client(
        &self,
        client_id: &str,
        client_secret: Option<&str>,
        redirect_uris: &[String],
        grant_types: &[String],
        scope: Option<&str>,
        token_endpoint_auth_method: Option<&str>,
    ) -> Result<()> {
        oauth_clients::upsert(
            &self.conn,
            client_id,
            client_secret,
            redirect_uris,
            grant_types,
            scope,
            token_endpoint_auth_method,
            now_secs(),
        )
    }

    pub fn read_oauth_client(&self, client_id: &str) -> Result<Option<OAuthClientRow>> {
        oauth_clients::read(&self.conn, client_id)
    }

    // ---- Auth codes ----

    #[allow(clippy::too_many_arguments)]
    pub fn write_auth_code(
        &self,
        code: &str,
        client_id: &str,
        scopes: &[String],
        code_challenge: &str,
        redirect_uri: &str,
        redirect_uri_provided_explicitly: bool,
        expires_at: f64,
        npub: Option<&str>,
    ) -> Result<()> {
        auth_codes::insert(
            &self.conn,
            code,
            client_id,
            scopes,
            code_challenge,
            redirect_uri,
            redirect_uri_provided_explicitly,
            expires_at,
            npub,
        )
    }

    pub fn read_auth_code(&self, code: &str) -> Result<Option<AuthCodeRow>> {
        auth_codes::read(&self.conn, code)
    }

    pub fn delete_auth_code(&self, code: &str) -> Result<()> {
        auth_codes::delete(&self.conn, code)
    }

    // ---- Access tokens ----

    pub fn write_access_token(
        &self,
        token: &str,
        client_id: &str,
        scopes: &[String],
        expires_at: Option<i64>,
        npub: Option<&str>,
    ) -> Result<()> {
        access_tokens::insert(&self.conn, token, client_id, scopes, expires_at, npub)
    }

    pub fn read_access_token(&self, token: &str) -> Result<Option<AccessTokenRow>> {
        access_tokens::read(&self.conn, token)
    }

    pub fn revoke_access_token(&self, token: &str) -> Result<()> {
        access_tokens::delete(&self.conn, token)
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        // AesKey zeroes itself on drop; explicit clear here is belt-and-braces.
        self.key = None;
    }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
