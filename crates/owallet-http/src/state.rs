//! Shared application state.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use dashmap::DashMap;
use owallet_db::Database;
use owallet_overpay::{OverpayClient, Pkce};

use crate::session::SessionStore;
use crate::EvmConfig;

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
    /// Stable identifier of the host the dashboard's bearer tokens are
    /// stored under (the OAuth issuer URL). Used by the dashboard handlers
    /// that look up stored Overpay tokens.
    pub host_key: String,
    /// Short-lived map of dashboard → Overpay OAuth handshakes in flight.
    pub pending_auth: PendingDashboardAuthMap,
}

impl AppState {
    #[must_use]
    pub fn new(
        db: Database,
        overpay: Arc<OverpayClient>,
        evm: EvmConfig,
        host_key: String,
    ) -> Self {
        Self {
            db: Arc::new(Mutex::new(db)),
            sessions: SessionStore::new(),
            overpay,
            evm,
            host_key,
            pending_auth: PendingDashboardAuthMap::default(),
        }
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
            host_key: self.host_key.clone(),
            pending_auth: PendingDashboardAuthMap::default(),
        }
    }
}
