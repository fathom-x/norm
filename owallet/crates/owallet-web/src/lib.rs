//! owallet in the browser.
//!
//! One axum [`Router`] answers what `owallet serve` answers for norm —
//! `/health`, `/v1/*` (the OpenAI-compatible provider), `/mcp` (tools,
//! provider-key bearer) — plus `/_mgmt/*`, a JSON API standing in for the
//! CLI verbs norm's bootstrap shells out to natively (`init`, `unlock`,
//! `generate`, `import`, `provider-key create --json`, `authorize`).
//!
//! The JS glue (`wasm` module, wasm32 only) exports `init(config)` and
//! `handle(request) -> Promise<Response>`: requests are fed to the router
//! with `tower::ServiceExt::oneshot`, response bodies stream back frame by
//! frame. Everything else here is target-independent, so the native test
//! suite drives the same router.
//!
//! Not in this graph by design: owallet-http (sockets, templates, cookie
//! sessions), owallet-evm / owallet-zcash (owallet-mcp is built without its
//! `evm` / `zcash` features; on-chain tools answer `unavailable_in_browser`)
//! and the CLI.

mod error;
mod mgmt;
#[cfg(all(target_family = "wasm", target_os = "unknown"))]
mod wasm;
#[cfg(all(target_family = "wasm", target_os = "unknown"))]
pub use wasm::{handle, import_memory_db, init};

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::extract::{Request, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use owallet_config::OverpayEndpoints;
use owallet_db::Database;
use owallet_mcp::transport::{mcp_router_with_auth, provider_key_bearer_auth, AuthResult};
use owallet_mcp::McpState;
use owallet_overpay::{OverpayClient, PkceLogin};
use serde::Deserialize;
use tower::ServiceExt;

pub use error::MgmtError;

/// Where the wallet database lives in the browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Storage {
    /// Origin-private file system (sync access handles — dedicated
    /// workers only). Survives reloads.
    Opfs,
    /// In memory; gone with the page.
    #[default]
    Memory,
}

/// What the JS host passes to `init(config)`. No env vars or dotenv files
/// are read in the browser — this is the whole configuration. Keys are
/// snake_case; camelCase aliases are accepted.
#[derive(Debug, Clone, Deserialize)]
pub struct WebConfig {
    /// Overpay API base URL. Optional only for an `env` with a built-in
    /// URL (`prod`).
    #[serde(default, alias = "railsUrl")]
    pub rails_url: Option<String>,
    /// Browser-facing Overpay URL (defaults to `rails_url`).
    #[serde(default, alias = "publicUrl")]
    pub public_url: Option<String>,
    /// Environment label reported by `/_mgmt/status` (`prod`/`dev`/`staging`).
    #[serde(default = "default_env")]
    pub env: String,
    /// `"opfs"` or `"memory"` (default).
    #[serde(default)]
    pub storage: Storage,
    /// Database file name inside the VFS (default `owallet.db`).
    #[serde(default, alias = "dbName")]
    pub db_name: Option<String>,
    /// OPFS directory the sync-access-handle pool lives in (default
    /// `.owallet-web`) — keep it apart from any other SQLite pool on the
    /// origin.
    #[serde(default, alias = "opfsDirectory")]
    pub opfs_directory: Option<String>,
}

fn default_env() -> String {
    "prod".into()
}

/// The open wallet database and the routers bound to it.
struct Opened {
    db: Arc<Mutex<Database>>,
    /// `/v1` + `/mcp`. Built once per open database: the `/v1` router
    /// caches the marketplace's listing tools for its lifetime.
    api: Router,
}

struct Inner {
    endpoints: OverpayEndpoints,
    overpay: Arc<OverpayClient>,
    db_path: PathBuf,
    opened: Mutex<Option<Opened>>,
    /// In-flight PKCE logins by `state`.
    logins: Mutex<HashMap<String, PkceLogin>>,
}

/// Shared state behind every route.
#[derive(Clone)]
pub struct App {
    inner: Arc<Inner>,
}

impl App {
    /// `db_path` is a VFS path in the browser, a file path natively.
    pub fn new(endpoints: OverpayEndpoints, db_path: PathBuf) -> Result<Self, MgmtError> {
        let overpay = OverpayClient::new(&endpoints.rails_url)
            .and_then(|c| c.with_public_url(&endpoints.public_url))
            .map_err(|e| MgmtError::bad_request("bad_config", format!("overpay url: {e}")))?;
        Ok(Self {
            inner: Arc::new(Inner {
                endpoints,
                overpay: Arc::new(overpay),
                db_path,
                opened: Mutex::new(None),
                logins: Mutex::new(HashMap::new()),
            }),
        })
    }

    /// Resolve a [`WebConfig`] into an app (the database itself opens on
    /// first use).
    pub fn from_config(config: &WebConfig, db_path: PathBuf) -> Result<Self, MgmtError> {
        let endpoints = OverpayEndpoints::from_values(
            &config.env,
            config.rails_url.as_deref(),
            config.public_url.as_deref(),
        )
        .ok_or_else(|| {
            MgmtError::bad_request(
                "bad_config",
                format!("rails_url is required for env `{}`", config.env),
            )
        })?;
        Self::new(endpoints, db_path)
    }

    pub fn endpoints(&self) -> &OverpayEndpoints {
        &self.inner.endpoints
    }

    pub fn overpay(&self) -> &Arc<OverpayClient> {
        &self.inner.overpay
    }

    /// The open database, opening it first if the file exists. `None`
    /// until `/_mgmt/init` created one.
    pub(crate) fn db(&self) -> Result<Option<Arc<Mutex<Database>>>, MgmtError> {
        let mut opened = self.lock_opened()?;
        if opened.is_none() && Database::exists(&self.inner.db_path) {
            let db = Database::open(&self.inner.db_path).map_err(MgmtError::db)?;
            *opened = Some(self.bind(db));
        }
        Ok(opened.as_ref().map(|o| o.db.clone()))
    }

    /// Install a freshly created database.
    pub(crate) fn install(&self, db: Database) -> Result<Arc<Mutex<Database>>, MgmtError> {
        let mut opened = self.lock_opened()?;
        let o = self.bind(db);
        let db = o.db.clone();
        *opened = Some(o);
        Ok(db)
    }

    pub(crate) fn db_path(&self) -> &std::path::Path {
        &self.inner.db_path
    }

    pub(crate) fn logins(&self) -> std::sync::MutexGuard<'_, HashMap<String, PkceLogin>> {
        self.inner
            .logins
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn lock_opened(&self) -> Result<std::sync::MutexGuard<'_, Option<Opened>>, MgmtError> {
        self.inner
            .opened
            .lock()
            .map_err(|_| MgmtError::internal("state mutex poisoned"))
    }

    /// The MCP state the `/v1` and `/mcp` routers share.
    pub(crate) fn mcp_state(&self, db: Arc<Mutex<Database>>) -> McpState {
        McpState::new(db, self.inner.overpay.clone())
    }

    fn bind(&self, db: Database) -> Opened {
        let db = Arc::new(Mutex::new(db));
        let mcp = self.mcp_state(db.clone());
        // Provider keys only: there is no local OAuth AS in the browser.
        let auth = provider_key_bearer_auth(db.clone(), |_| AuthResult::Invalid);
        let api = Router::new()
            .nest("/v1", owallet_mcp::openai_compat::router(mcp.clone()))
            .nest("/mcp", mcp_router_with_auth(mcp, auth));
        Opened { db, api }
    }

    fn api_router(&self) -> Result<Option<Router>, MgmtError> {
        self.db()?;
        Ok(self.lock_opened()?.as_ref().map(|o| o.api.clone()))
    }
}

/// The whole browser-side owallet surface.
pub fn router(app: App) -> Router {
    Router::new()
        .route("/health", get(health))
        .nest("/_mgmt", mgmt::router(app.clone()))
        .fallback(api)
        .with_state(app)
}

/// Same shape as owallet-http's `/health`; norm reads `version` to decide
/// whether the server accepts provider keys on `/mcp`.
async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "name": "owallet",
        "version": owallet_mcp::VERSION,
    }))
}

/// `/v1/*` and `/mcp`, once a database exists.
async fn api(State(app): State<App>, req: Request) -> Response {
    let path = req.uri().path();
    if !(path == "/mcp" || path.starts_with("/mcp/") || path.starts_with("/v1/")) {
        return MgmtError::not_found(path).into_response();
    }
    match app.api_router() {
        Ok(Some(api)) => match api.oneshot(req).await {
            Ok(resp) => resp,
            Err(never) => match never {},
        },
        Ok(None) => MgmtError::not_initialized().into_response(),
        Err(e) => e.into_response(),
    }
}
