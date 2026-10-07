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
    /// Key the Overpay bearer is filed under in the `tokens` table. Derived
    /// from the Overpay API base URL (see [`owallet_overpay::host_key`]), so
    /// a wallet linked from the `owallet authorize` CLI and one linked from
    /// the dashboard resolve to the same row.
    pub host_key: String,
    /// Host keys an older build may have filed the bearer under. Read
    /// through (and migrated forward) when [`Self::host_key`] misses.
    pub legacy_host_keys: Vec<String>,
    /// EVM JSON-RPC URL (e.g. https://mainnet.base.org). Used by the
    /// USDC-send tool.
    pub evm_rpc_url: String,
    /// CAIP-2 chain id (e.g. eip155:8453). Determines the USDC contract +
    /// decimals used by the USDC-send tool.
    pub evm_network: String,
    /// lightwalletd server (operator alias or host:port) for Zcash.
    pub zcash_lightwalletd: String,
    /// Zcash network name (`mainnet`/`testnet`).
    pub zcash_network: String,
    /// Set when the request authenticated with a `/v1`-style `owk_`
    /// provider key rather than an OAuth token or dashboard session.
    /// Purchases made by such a session are gated on the key's scopes and
    /// recorded against its daily budget, exactly as `/v1` does — one
    /// credential, one budget, both surfaces.
    pub provider_key_id: Option<String>,
    /// Whether that provider key's scopes include `spend`. Meaningless
    /// unless [`Self::provider_key_id`] is set.
    pub provider_key_can_spend: bool,
    /// The opaque label for the session (conversation) this call belongs
    /// to, when the caller identified one: orders it places carry it as
    /// `client_session_id`, and `get_wallet_orders` lists by it. Always
    /// the output of [`Self::order_session_label`], never a client's raw id.
    pub order_session: Option<String>,
}

/// An owned auth strategy with its data living long enough for one tool
/// call. Borrow via [`OwnedAuth::as_auth`] to feed into [`OverpayClient`].
pub enum OwnedAuth {
    Bearer(String),
    Nip98(PrivateKey),
    /// The bearer token plus the wallet key, which signs every request
    /// (`X-Nostr-Signature`; POST bodies covered by the NIP-98 `payload`
    /// tag) — Overpay's proof that the keyholder authorized each spend.
    BearerSigned(String, PrivateKey),
}

impl OwnedAuth {
    /// The auth a wallet has on hand: its stored Overpay bearer, signed by
    /// the wallet key when the seed is readable ([`Self::BearerSigned`]),
    /// else a NIP-98 key derived from its seed. `None` when it has neither.
    /// [`McpState::resolve_owned_auth`] is the same choice with its
    /// failures spelled out.
    #[must_use]
    pub fn from_stored(token: Option<String>, seed: Option<&str>) -> Option<Self> {
        let key = seed.and_then(|s| derive_from_stored_seed(s).ok());
        match (token, key) {
            (Some(token), Some(key)) => Some(Self::BearerSigned(token, key)),
            (Some(token), None) => Some(Self::Bearer(token)),
            (None, key) => key.map(Self::Nip98),
        }
    }

    pub fn as_auth(&self) -> Auth<'_> {
        match self {
            Self::Bearer(t) => Auth::Bearer(t),
            Self::Nip98(sk) => Auth::Nip98(sk),
            Self::BearerSigned(t, sk) => Auth::BearerSigned(t, sk),
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
    /// The token-store key is derived from `overpay` rather than passed in —
    /// the bearer belongs to the Overpay host, not to whatever local server
    /// happens to be hosting these tools.
    pub fn new(db: Arc<Mutex<Database>>, overpay: Arc<OverpayClient>) -> Self {
        let host_key = overpay.host_key();
        Self {
            db,
            overpay,
            active_npub: None,
            host_key,
            legacy_host_keys: Vec::new(),
            evm_rpc_url: "https://mainnet.base.org".to_string(),
            evm_network: "eip155:8453".to_string(),
            zcash_lightwalletd: "zecrocks".to_string(),
            zcash_network: "mainnet".to_string(),
            provider_key_id: None,
            provider_key_can_spend: false,
            order_session: None,
        }
    }

    /// The label for a client's session id: HMAC-SHA256(per-install secret,
    /// id), hex. The label lands in Overpay's database (and, as an
    /// OpenRouter `session_id`, in OpenRouter's logs), so it must be unique
    /// across buyers, reveal nothing about the wallet, and not be linkable
    /// to the client's own id — hence keyed by a secret, not a plain hash.
    /// `None` for a blank id or when the secret cannot be read.
    pub fn order_session_label(&self, client_session_id: &str) -> Option<String> {
        let id = client_session_id.trim();
        if id.is_empty() {
            return None;
        }
        let secret = self.db.lock().ok()?.session_id_secret().ok()?;
        Some(session_label(&secret, id))
    }

    /// Returns a clone of this state bound to a session label (see
    /// [`Self::order_session`]).
    pub fn with_order_session(&self, label: Option<String>) -> Self {
        Self {
            order_session: label,
            ..self.clone()
        }
    }

    /// Register host keys to read through when [`Self::host_key`] misses —
    /// token layouts written by older builds.
    pub fn with_legacy_host_keys(mut self, keys: Vec<String>) -> Self {
        self.legacy_host_keys = keys.into_iter().filter(|k| *k != self.host_key).collect();
        self
    }

    pub fn with_evm(mut self, rpc_url: String, network: String) -> Self {
        self.evm_rpc_url = rpc_url;
        self.evm_network = network;
        self
    }

    pub fn with_zcash(mut self, lightwalletd: String, network: String) -> Self {
        self.zcash_lightwalletd = lightwalletd;
        self.zcash_network = network;
        self
    }

    /// Parse the configured Zcash network.
    pub fn zcash_net(&self) -> Result<owallet_zcash::Network, owallet_zcash::ZcashError> {
        owallet_zcash::Network::parse(&self.zcash_network)
    }

    /// Per-wallet Zcash data directory (`<data dir>/<npub>/zcash/`).
    pub fn zcash_data_dir(
        &self,
        npub: &str,
    ) -> Result<std::path::PathBuf, owallet_zcash::ZcashError> {
        owallet_zcash::data_dir_for(npub)
    }

    /// Returns a clone of this state with `active_npub` set to `npub`.
    pub fn with_npub(&self, npub: Option<String>) -> Self {
        Self {
            db: self.db.clone(),
            overpay: self.overpay.clone(),
            active_npub: npub,
            host_key: self.host_key.clone(),
            legacy_host_keys: self.legacy_host_keys.clone(),
            evm_rpc_url: self.evm_rpc_url.clone(),
            evm_network: self.evm_network.clone(),
            zcash_lightwalletd: self.zcash_lightwalletd.clone(),
            zcash_network: self.zcash_network.clone(),
            provider_key_id: self.provider_key_id.clone(),
            provider_key_can_spend: self.provider_key_can_spend,
            order_session: self.order_session.clone(),
        }
    }

    /// Returns a clone of this state bound to a provider key: requests
    /// run as the key's wallet, and purchase tools account against the
    /// key's daily budget under its scopes.
    pub fn with_provider_key(&self, key_id: String, can_spend: bool) -> Self {
        let mut state = self.clone();
        state.provider_key_id = Some(key_id);
        state.provider_key_can_spend = can_spend;
        state
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

    /// Look up the stored Overpay bearer token for the given npub, migrating
    /// a token filed under a legacy host key forward on first hit.
    pub fn read_overpay_token(&self, npub: &str) -> Option<String> {
        let db = self.db.lock().ok()?;
        db.read_token_migrating(npub, &self.host_key, &self.legacy_host_keys)
            .ok()
            .flatten()
    }

    /// Resolve the auth strategy for an Overpay request:
    ///
    /// 1. If a Bearer token is stored for the active wallet, use it — and
    ///    when the wallet key is available (the DB is unlocked, as it is
    ///    under `owallet serve`), sign every request with it too
    ///    ([`OwnedAuth::BearerSigned`]): each spend then carries the
    ///    keyholder's signature over its exact body.
    /// 2. Otherwise fall back to NIP-98 by decrypting the wallet seed,
    ///    deriving the secp256k1 key, and signing each request.
    ///
    /// Returns the npub used along with the owned auth material.
    pub fn resolve_owned_auth(&self) -> Result<(String, OwnedAuth), ResolveAuthError> {
        let npub = self.resolve_npub().ok_or(ResolveAuthError::NoWallet)?;
        if let Some(token) = self.read_overpay_token(&npub) {
            return Ok(match self.wallet_key(&npub) {
                Some(sk) => (npub, OwnedAuth::BearerSigned(token, sk)),
                None => (npub, OwnedAuth::Bearer(token)),
            });
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

    /// The wallet's signing key (the npub's, which is also its EVM key), if
    /// the DB is unlocked and the seed readable.
    fn wallet_key(&self, npub: &str) -> Option<PrivateKey> {
        let seed = {
            let db = self.db.lock().ok()?;
            if !db.is_unlocked() {
                return None;
            }
            db.read_seed(npub).ok().flatten()?
        };
        derive_from_stored_seed(&seed).ok()
    }
}

/// HMAC-SHA256(secret, id) as hex (64 chars): the one derivation behind
/// both an order's `client_session_id` and the OpenRouter `session_id`, so
/// the two always agree for a conversation.
pub(crate) fn session_label(secret: &[u8], client_session_id: &str) -> String {
    use hmac::{Hmac, Mac};
    let mut mac =
        Hmac::<sha2::Sha256>::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(b"owallet/openrouter-session-id/v1\0");
    mac.update(client_session_id.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}
