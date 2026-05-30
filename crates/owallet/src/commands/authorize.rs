//! `owallet authorize` — link the current wallet to Overpay via OAuth PKCE.
//!
//! Ports the Python flow in `wallet_mcp/cli.py:664-` plus the
//! `_wallet_authorize_callback` handler in `server.py:809`. We bind a
//! local axum callback server on a free port, register a public OAuth
//! client with the Rails app, open the user's browser to the authorize URL,
//! and block until the callback delivers a code (or a timeout fires).

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Html;
use axum::routing::get;
use axum::Router;
use owallet_crypto::{derive_from_mnemonic, Mnemonic, PrivateKey, EVM_HD_PATH};
use owallet_db::default_db_path;
use owallet_overpay::models::OAuthRegisterRequest;
use owallet_overpay::Pkce;
use serde::Deserialize;
use tokio::sync::{oneshot, Mutex};

use super::overpay::{block_on, client as overpay_client, host_key};
use super::{open_unlock, CmdError, Result};

const CALLBACK_TIMEOUT: Duration = Duration::from_secs(300);

pub fn run() -> Result<()> {
    let db = open_unlock(&default_db_path())?;
    let npub = db
        .read_default_npub()?
        .ok_or_else(|| CmdError::BadInput("no default wallet — run `owallet select`".into()))?;
    let seed = db
        .read_seed(&npub)?
        .ok_or_else(|| CmdError::NotFound(npub.clone()))?;
    let sk = derive_private_key(&seed)?;
    drop(seed);

    let overpay = overpay_client()?;
    let host = host_key();

    block_on(async move {
        let pkce = Pkce::generate();

        // Bind first to learn the port; then we know what redirect_uri to
        // register with the OAuth provider.
        let listener =
            tokio::net::TcpListener::bind(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0))
                .await
                .map_err(CmdError::Io)?;
        let local_addr = listener.local_addr().map_err(CmdError::Io)?;
        let redirect_uri = format!("http://127.0.0.1:{}/callback", local_addr.port());

        // Register an ephemeral public OAuth client.
        let reg = overpay
            .register_oauth_client(&OAuthRegisterRequest {
                client_name: "owallet".into(),
                redirect_uris: vec![redirect_uri.clone()],
                grant_types: vec!["authorization_code".into()],
                response_types: vec!["code".into()],
                scope: Some("wallet".into()),
                token_endpoint_auth_method: Some("none".into()),
            })
            .await?;

        let auth_url = overpay.authorize_url(
            &reg.client_id,
            &redirect_uri,
            &pkce.state,
            &pkce.challenge,
            "wallet",
        )?;

        // One-shot channel for the callback to hand back the (code, state).
        let (tx, rx) = oneshot::channel::<CallbackResult>();
        let inbound = Arc::new(InboundState {
            expected_state: pkce.state.clone(),
            tx: Mutex::new(Some(tx)),
        });

        let app = Router::new()
            .route("/callback", get(callback))
            .with_state(inbound);

        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        println!("Opening the Overpay authorize URL in your browser…");
        println!("If it doesn't open automatically, visit:\n  {auth_url}");
        let _ = open::that(auth_url.as_str());

        let cb = tokio::time::timeout(CALLBACK_TIMEOUT, rx)
            .await
            .map_err(|_| {
                CmdError::OauthCallback(format!(
                    "no callback within {}s — try again",
                    CALLBACK_TIMEOUT.as_secs()
                ))
            })?
            .map_err(|_| CmdError::OauthCallback("callback channel closed".into()))?;

        server.abort();

        let code = match cb {
            CallbackResult::Code(c) => c,
            CallbackResult::Error(msg) => return Err(CmdError::OauthCallback(msg)),
        };

        let token = overpay
            .exchange_code(&reg.client_id, &code, &pkce.verifier, &redirect_uri)
            .await?;
        db.write_token(&npub, &host, &token.access_token, "overpay-oauth")?;

        // Confirm by fetching the account; cache the username for offline view.
        let info = overpay
            .account(owallet_overpay::Auth::Bearer(&token.access_token))
            .await?;
        if let Some(u) = info.username.as_ref() {
            db.cache_wallet_username(&npub, u)?;
        }

        println!("Authorized {npub}");
        if let Some(u) = info.username.as_deref() {
            println!("  linked to Overpay user: {u}");
        }
        if let Some(n) = info.account_number.as_deref() {
            println!("  account number:        {n}");
        }
        drop(sk); // explicit final drop — key zeroized
        Ok(())
    })
}

fn derive_private_key(seed: &str) -> Result<PrivateKey> {
    if seed.split_whitespace().count() >= 12 {
        let m = Mnemonic::parse(seed)?;
        Ok(derive_from_mnemonic(&m, EVM_HD_PATH)?)
    } else {
        Ok(PrivateKey::from_hex(seed)?)
    }
}

// ---- Callback handler ----

enum CallbackResult {
    Code(String),
    Error(String),
}

struct InboundState {
    expected_state: String,
    tx: Mutex<Option<oneshot::Sender<CallbackResult>>>,
}

#[derive(Debug, Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

async fn callback(
    State(state): State<Arc<InboundState>>,
    Query(q): Query<CallbackQuery>,
) -> (StatusCode, Html<&'static str>) {
    let result = if let Some(err) = q.error {
        CallbackResult::Error(format!(
            "{err}: {}",
            q.error_description.unwrap_or_default()
        ))
    } else if q.state.as_deref() != Some(state.expected_state.as_str()) {
        CallbackResult::Error("state mismatch (CSRF protection)".into())
    } else if let Some(code) = q.code {
        CallbackResult::Code(code)
    } else {
        CallbackResult::Error("callback missing both `code` and `error`".into())
    };

    let ok = matches!(result, CallbackResult::Code(_));
    if let Some(tx) = state.tx.lock().await.take() {
        let _ = tx.send(result);
    }
    let body = if ok {
        "<h2>Authorization successful.</h2><p>You can close this tab.</p>"
    } else {
        "<h2>Authorization failed.</h2><p>See terminal output.</p>"
    };
    (StatusCode::OK, Html(body))
}
