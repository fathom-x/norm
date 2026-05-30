//! axum-based HTTP dashboard for owallet.
//!
//! Port of the `/wallet/*` routes from `wallet_mcp/server.py`. This phase
//! covers the password-protected dashboard only — the MCP server, OAuth
//! provider, and Overpay-API-backed views land in later phases.

mod dashboard;
mod error;
pub mod oauth_as;
mod session;
mod state;
mod templates;

pub use oauth_as::{OAuthAsState, PendingMap};
pub use session::{SessionRole, SessionStore, WebSession};
pub use state::{AppState, PendingDashboardAuth, PendingDashboardAuthMap};

use std::future::Future;
use std::net::SocketAddr;

/// Bind and serve the dashboard on `addr` with graceful shutdown driven by
/// `shutdown`. Returns once the shutdown signal fires.
pub async fn serve<F>(addr: SocketAddr, state: AppState, shutdown: F) -> Result<(), std::io::Error>
where
    F: Future<Output = ()> + Send + 'static,
{
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
}

use std::sync::Arc;

use axum::{
    routing::{get, post},
    Router,
};
use owallet_mcp::transport::{mcp_router_with_auth, AuthResult, BearerAuthCheck};
use owallet_mcp::McpState;

/// Build the axum router with just the dashboard routes. The same router
/// can be driven from `axum_test::TestServer` in integration tests.
pub fn build_router(state: AppState) -> Router {
    dashboard_routes(state.clone()).with_state(state)
}

/// EVM config injected into [`build_full_router`]. The MCP `send_usdc`
/// tool reads `rpc_url` + `network` (CAIP-2 chain id) from here.
#[derive(Debug, Clone)]
pub struct EvmConfig {
    pub rpc_url: String,
    pub network: String,
}

impl Default for EvmConfig {
    fn default() -> Self {
        Self {
            rpc_url: "https://mainnet.base.org".to_string(),
            network: "eip155:8453".to_string(),
        }
    }
}

/// Build the full router: dashboard + OAuth AS + `/mcp`. `issuer_url` is
/// the externally-reachable base URL that goes into the OAuth metadata
/// document (e.g. `http://127.0.0.1:8765`). The Overpay client + EVM
/// config + host key are read directly from [`AppState`] — they were
/// threaded into the state at construction time so the dashboard
/// handlers (send / overpay-login / authorize) share them with the
/// MCP server.
pub fn build_full_router(state: AppState, issuer_url: String) -> Router {
    let oauth = OAuthAsState {
        app: state.clone(),
        issuer_url: issuer_url.clone(),
        pending: PendingMap::default(),
    };

    let mcp = McpState::new(
        state.db.clone(),
        state.overpay.clone(),
        state.host_key.clone(),
    )
    .with_evm(state.evm.rpc_url.clone(), state.evm.network.clone());
    let app_for_auth = state.clone();
    let auth: BearerAuthCheck = Arc::new(move |bearer: Option<&str>| match bearer {
        Some(b) => oauth_as::lookup_token(&app_for_auth, b),
        None => AuthResult::Anonymous,
    });

    dashboard_routes(state.clone())
        .with_state(state)
        .merge(oauth_as::router(oauth))
        .nest("/mcp", mcp_router_with_auth(mcp, auth))
}

fn dashboard_routes(state: AppState) -> Router<AppState> {
    let _ = state; // routes use State<AppState> via the closures.
    Router::new()
        .route("/", get(dashboard::index::redirect_to_wallet))
        .route(
            "/wallet/login",
            get(dashboard::login::login_get).post(dashboard::login::login_post),
        )
        .route("/wallet/logout", post(dashboard::login::logout_post))
        .route("/wallet", get(dashboard::index::dashboard))
        .route(
            "/wallet/generate",
            get(dashboard::generate::generate_get).post(dashboard::generate::generate_post),
        )
        .route(
            "/wallet/import",
            get(dashboard::import::import_get).post(dashboard::import::import_post),
        )
        .route(
            "/wallet/password",
            get(dashboard::password::password_get).post(dashboard::password::password_post),
        )
        .route("/wallet/select", post(dashboard::select::select_post))
        .route("/wallet/delete", post(dashboard::delete::delete_post))
        .route("/wallet/send", post(dashboard::send::send_post))
        .route(
            "/wallet/overpay-login",
            get(dashboard::oauth::overpay_login_get),
        )
        .route("/wallet/authorize", get(dashboard::oauth::authorize_get))
        .route(
            "/wallet/authorize/callback",
            get(dashboard::oauth::authorize_callback),
        )
        .route("/wallet/purchases", get(dashboard::purchases::list_get))
        .route(
            "/wallet/purchases/sync",
            post(dashboard::purchases::sync_post),
        )
        .route(
            "/wallet/purchases/{order_id}",
            get(dashboard::purchases::detail_get),
        )
}
