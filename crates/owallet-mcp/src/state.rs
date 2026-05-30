//! Shared state injected into every MCP tool call.

use std::sync::{Arc, Mutex};

use owallet_crypto::{derive_from_stored_seed, HdError, PrivateKey};
use owallet_db::Database;
use owallet_overpay::{Auth, OverpayClient};

/// Everything a tool handler needs to do its work: the encrypted DB
/// (for stored bearer tokens + default npub lookup), the Rails REST client,
/// and the npub the request is bound to (set by the auth layer).
#[derive(Clone)]
pub struct McpState {
    pub db: Arc<Mutex<Database>>,
    pub overpay: Arc<OverpayClient>,
    /// Best-effort npub associated with the bearer token used for this
    /// request. `None` means anonymous (only public tools allowed).
    pub active_npub: Option<String>,
    /// Stable identifier of the host the bearer token was issued by. Used
    /// to look up the Overpay token in the `tokens` table.
    pub host_key: String,
    /// EVM JSON-RPC URL (e.g. https://mainnet.base.org). Used by the
    /// USDC-send tool.
    pub evm_rpc_url: String,
    /// CAIP-2 chain id (e.g. eip155:8453). Determines the USDC contract +
    /// decimals used by the USDC-send tool.
    pub evm_network: String,
}

/// An owned auth strategy with its data living long enough for one tool
/// call. Borrow via [`OwnedAuth::as_auth`] to feed into [`OverpayClient`].
pub enum OwnedAuth {
    Bearer(String),
    Nip98(PrivateKey),
}

impl OwnedAuth {
    pub fn as_auth(&self) -> Auth<'_> {
        match self {
            Self::Bearer(t) => Auth::Bearer(t),
            Self::Nip98(sk) => Auth::Nip98(sk),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ResolveAuthError {
    #[error("no wallet selected — run `owallet select` or pass a wallet identifier")]
    NoWallet,
    #[error("db is locked — no per-wallet keys available for NIP-98 signing")]
    DbLocked,
    #[error("seed lookup failed: {0}")]
    Db(#[from] owallet_db::DbError),
    #[error("hd derivation: {0}")]
    Hd(#[from] HdError),
    #[error("internal: {0}")]
    Internal(String),
}

impl McpState {
    pub fn new(db: Arc<Mutex<Database>>, overpay: Arc<OverpayClient>, host_key: String) -> Self {
        Self {
            db,
            overpay,
            active_npub: None,
            host_key,
            evm_rpc_url: "https://mainnet.base.org".to_string(),
            evm_network: "eip155:8453".to_string(),
        }
    }

    pub fn with_evm(mut self, rpc_url: String, network: String) -> Self {
        self.evm_rpc_url = rpc_url;
        self.evm_network = network;
        self
    }

    /// Returns a clone of this state with `active_npub` set to `npub`.
    pub fn with_npub(&self, npub: Option<String>) -> Self {
        Self {
            db: self.db.clone(),
            overpay: self.overpay.clone(),
            active_npub: npub,
            host_key: self.host_key.clone(),
            evm_rpc_url: self.evm_rpc_url.clone(),
            evm_network: self.evm_network.clone(),
        }
    }

    /// Resolve the npub this tool call should sign as: the auth-bound npub
    /// if set, otherwise the DB default.
    pub fn resolve_npub(&self) -> Option<String> {
        if let Some(n) = &self.active_npub {
            return Some(n.clone());
        }
        let db = self.db.lock().ok()?;
        db.read_default_npub().ok().flatten()
    }

    /// Look up the stored Overpay bearer token for the given npub.
    pub fn read_overpay_token(&self, npub: &str) -> Option<String> {
        let db = self.db.lock().ok()?;
        db.read_token(npub, &self.host_key).ok().flatten()
    }

    /// Resolve the auth strategy for an Overpay request:
    ///
    /// 1. If a Bearer token is stored for the active wallet, use it.
    /// 2. Otherwise fall back to NIP-98 by decrypting the wallet seed,
    ///    deriving the secp256k1 key, and signing each request.
    ///
    /// Returns the npub used along with the owned auth material.
    pub fn resolve_owned_auth(&self) -> Result<(String, OwnedAuth), ResolveAuthError> {
        let npub = self.resolve_npub().ok_or(ResolveAuthError::NoWallet)?;
        if let Some(token) = self.read_overpay_token(&npub) {
            return Ok((npub, OwnedAuth::Bearer(token)));
        }
        let seed = {
            let db = self
                .db
                .lock()
                .map_err(|e| ResolveAuthError::Internal(format!("db mutex: {e}")))?;
            if !db.is_unlocked() {
                return Err(ResolveAuthError::DbLocked);
            }
            db.read_seed(&npub)?.ok_or(ResolveAuthError::NoWallet)?
        };
        let sk = derive_from_stored_seed(&seed)?;
        Ok((npub, OwnedAuth::Nip98(sk)))
    }
}
