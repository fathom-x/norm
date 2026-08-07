//! Shared application state.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use dashmap::DashMap;
use owallet_db::Database;
use owallet_overpay::{OverpayClient, Pkce};

use crate::session::SessionStore;
use crate::{EvmConfig, ZcashConfig};

/// Cookie name for the dashboard session token. Matches the Python
/// implementation (`server.py:199`).
pub const SESSION_COOKIE: &str = "owallet_session";

/// Cookie name for the short-lived dashboard → Overpay-OAuth handoff.
/// Set when the user starts the browser-OAuth flow, consumed (and cleared)
/// when the callback fires.
pub const PENDING_AUTH_COOKIE: &str = "owallet_pending_auth";

/// One in-flight dashboard → Overpay-OAuth handshake. Tied to a specific
/// wallet npub so the callback completes the link for the wallet that
/// started it, even if the active default has changed since.
#[derive(Debug, Clone)]
pub struct PendingDashboardAuth {
    pub pkce: Pkce,
    pub client_id: String,
    pub redirect_uri: String,
    pub started_npub: String,
    pub expires_at: Instant,
}

/// Thread-safe pending-auth map. Clone is cheap (Arc'd) so the dashboard
/// handlers can share it across requests.
#[derive(Clone, Default)]
pub struct PendingDashboardAuthMap(pub Arc<DashMap<String, PendingDashboardAuth>>);

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Mutex<Database>>,
    pub sessions: SessionStore,
    /// REST client for the Overpay Rails API. Shared with the MCP server
    /// so dashboard handlers (send / overpay-login / authorize) use the
    /// same endpoint configuration.
    pub overpay: Arc<OverpayClient>,
    /// JSON-RPC URL + CAIP-2 chain id used by the dashboard `POST /wallet/send`
    /// handler (and propagated into the MCP server's `send_usdc` tool).
    pub evm: EvmConfig,
    /// lightwalletd + network used by the dashboard `POST /wallet/send` ZEC
    /// branch and propagated into the MCP `send_zcash`/`sync_zcash` tools.
    pub zcash: ZcashConfig,
    /// Key the Overpay bearer tokens are filed under in the wallet DB.
    /// Derived from the Overpay API base URL (see
    /// [`owallet_overpay::host_key`]) so the dashboard, the MCP tools and
    /// the `owallet authorize` CLI all read the same row.
    pub host_key: String,
    /// Externally-reachable base URL of *this* dashboard — the OAuth issuer
    /// (e.g. `http://127.0.0.1:8766`). Used to build the dashboard's own
    /// OAuth redirect URI. Deliberately **not** a token-store key: two
    /// `owallet serve` instances pointed at the same Overpay host share one
    /// bearer, they don't each need their own authorization.
    pub public_base_url: String,
    /// Host keys an older build may have filed this wallet's Overpay bearer
    /// under. Read through (and migrated forward) when [`Self::host_key`]
    /// misses.
    pub legacy_host_keys: Vec<String>,
    /// Short-lived map of dashboard → Overpay OAuth handshakes in flight.
    pub pending_auth: PendingDashboardAuthMap,
}

impl AppState {
    #[must_use]
    pub fn new(
        db: Database,
        overpay: Arc<OverpayClient>,
        evm: EvmConfig,
        public_base_url: String,
    ) -> Self {
        Self::new_shared(
            Arc::new(Mutex::new(db)),
            overpay,
            evm,
            ZcashConfig::default(),
            public_base_url,
        )
    }

    /// Build a state around an already-shared DB handle. `owallet serve`
    /// uses this to run several servers over one unlocked `Database`.
    ///
    /// `public_base_url` is this dashboard's externally-reachable base URL.
    /// The token-store key is derived from `overpay` rather than passed in,
    /// so a caller can't accidentally file bearers under the local dashboard
    /// URL the way `serve` once did.
    #[must_use]
    pub fn new_shared(
        db: Arc<Mutex<Database>>,
        overpay: Arc<OverpayClient>,
        evm: EvmConfig,
        zcash: ZcashConfig,
        public_base_url: String,
    ) -> Self {
        let host_key = overpay.host_key();
        let public_base_url = public_base_url.trim_end_matches('/').to_string();
        let legacy_host_keys = if public_base_url == host_key {
            Vec::new()
        } else {
            vec![public_base_url.clone()]
        };
        Self {
            db,
            sessions: SessionStore::new(),
            overpay,
            evm,
            zcash,
            host_key,
            public_base_url,
            legacy_host_keys,
            pending_auth: PendingDashboardAuthMap::default(),
        }
    }

    /// Register another host key to read through when [`Self::host_key`]
    /// misses. Used for token layouts written by older builds.
    #[must_use]
    pub fn with_legacy_host_key(mut self, key: String) -> Self {
        if key != self.host_key && !self.legacy_host_keys.contains(&key) {
            self.legacy_host_keys.push(key);
        }
        self
    }

    /// Read the stored Overpay bearer for `npub`, migrating a token filed
    /// under a legacy host key forward on first hit.
    pub fn read_overpay_token(&self, npub: &str) -> Result<Option<String>, crate::error::AppError> {
        let db = self
            .db
            .lock()
            .map_err(|e| crate::error::AppError::Internal(format!("db mutex poisoned: {e}")))?;
        Ok(db.read_token_migrating(npub, &self.host_key, &self.legacy_host_keys)?)
    }

    /// Convenience constructor for tests that don't exercise the live
    /// Overpay / EVM surface. The Overpay client points at a placeholder
    /// URL that's never actually reached; the EVM config carries the
    /// in-crate defaults.
    #[must_use]
    pub fn for_test(db: Database) -> Self {
        let overpay =
            Arc::new(OverpayClient::new("http://127.0.0.1:1").expect("placeholder url parses"));
        Self::new(db, overpay, EvmConfig::default(), "test-host".to_string())
    }

    /// Build a sibling state that shares the underlying encrypted DB (so
    /// every wallet is reachable) but has its own empty session map. Used
    /// by `owallet serve` to spawn one server per `.owallet` config —
    /// signing in on the dev server doesn't grant access to the prod
    /// server even though both serve the same wallets.
    #[must_use]
    pub fn fork_with_fresh_sessions(&self) -> Self {
        Self {
            db: self.db.clone(),
            sessions: SessionStore::new(),
            overpay: self.overpay.clone(),
            evm: self.evm.clone(),
            zcash: self.zcash.clone(),
            host_key: self.host_key.clone(),
            public_base_url: self.public_base_url.clone(),
            legacy_host_keys: self.legacy_host_keys.clone(),
            pending_auth: PendingDashboardAuthMap::default(),
        }
    }
}
