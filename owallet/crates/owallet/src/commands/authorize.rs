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
        // Bind first to learn the port; then we know what redirect_uri to
        // register with the OAuth provider.
        let listener =
            tokio::net::TcpListener::bind(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0))
                .await
                .map_err(CmdError::Io)?;
        let local_addr = listener.local_addr().map_err(CmdError::Io)?;
        let redirect_uri = format!("http://127.0.0.1:{}/callback", local_addr.port());

        // Register an ephemeral public OAuth client and build the
        // authorize URL (PKCE).
        let login = overpay
            .begin_pkce_login("owallet", &redirect_uri, "wallet")
            .await?;
        let auth_url = login.authorize_url.clone();

        // One-shot channel for the callback to hand back the (code, state).
        let (tx, rx) = oneshot::channel::<CallbackResult>();
        let inbound = Arc::new(InboundState {
            expected_state: login.pkce.state.clone(),
            tx: Mutex::new(Some(tx)),
        });

        let app = Router::new()
            .route("/callback", get(callback))
            .with_state(inbound.clone());

        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        println!("Opening the Overpay authorize URL in your browser…");
        println!("If it doesn't open automatically, visit:\n  {auth_url}");
        println!();
        println!("Logging in from another device (e.g. this is an SSH session)?");
        println!("After you approve, your browser lands on a page that fails to load —");
        println!("its address starts with {redirect_uri}. Paste that whole address");
        println!("here (or just its `code` value) and press Enter.");
        // Paste path: runs alongside the loopback callback; whichever
        // delivers first wins. A plain std thread (not spawn_blocking) so a
        // read still pending on stdin never holds the process open.
        spawn_paste_reader(inbound.clone());
        // Detached: never wait for the browser-opener to exit. On a
        // current-thread runtime a blocking `open::that` would monopolise the
        // only thread and starve the callback server task below — deadlocking
        // the whole flow in headless environments.
        let _ = open::that_detached(auth_url.as_str());

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

        let token = overpay.finish_pkce_login(&login, &code).await?;
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

// ---- Pasted callback (remote / headless login) ----

/// Read pasted callback URLs (or bare codes) from stdin until one is valid
/// or the flow has already completed. Invalid input explains itself and
/// keeps waiting rather than failing the login; EOF (no terminal attached)
/// just ends the reader and leaves the loopback callback to finish.
fn spawn_paste_reader(inbound: Arc<InboundState>) {
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut line = String::new();
        loop {
            line.clear();
            match stdin.read_line(&mut line) {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            if line.trim().is_empty() {
                continue;
            }
            match parse_pasted(&line, &inbound.expected_state) {
                Ok(result) => {
                    if let Some(tx) = inbound.tx.blocking_lock().take() {
                        let _ = tx.send(result);
                    }
                    return;
                }
                Err(why) => eprintln!("{why}"),
            }
        }
    });
}

/// Interpret what the user pasted: the full callback address, just its
/// query string, or the bare `code`. A pasted `state` must match this
/// login's (CSRF protection, same as the loopback handler); a bare code
/// carries no state, which is still safe — PKCE binds the code to this
/// process's verifier, so a code from anywhere else cannot be exchanged.
fn parse_pasted(input: &str, expected_state: &str) -> std::result::Result<CallbackResult, String> {
    let text = input.trim().trim_matches(|c| c == '"' || c == '\'');
    if text.contains("code_challenge=") {
        return Err(
            "That's the login address itself — open it in a browser, approve, then paste the \
             address the browser ends up on."
                .into(),
        );
    }
    let query = match text.split_once('?') {
        Some((_, q)) => Some(q),
        None if text.contains('=') => Some(text),
        None => None,
    };
    let Some(query) = query else {
        let looks_like_code = text.len() >= 16 && !text.chars().any(char::is_whitespace);
        return if looks_like_code {
            Ok(CallbackResult::Code(text.to_string()))
        } else {
            Err("That doesn't look like the callback address or a code — try again.".into())
        };
    };
    let params: std::collections::HashMap<String, String> =
        url::form_urlencoded::parse(query.split('#').next().unwrap_or(query).as_bytes())
            .into_owned()
            .collect();
    if let Some(err) = params.get("error") {
        return Ok(CallbackResult::Error(format!(
            "{err}: {}",
            params
                .get("error_description")
                .map(String::as_str)
                .unwrap_or_default()
        )));
    }
    if let Some(state) = params.get("state") {
        if state != expected_state {
            return Err(
                "That address is from a different login attempt (state doesn't match) — \
                 paste the one from this login."
                    .into(),
            );
        }
    }
    match params.get("code") {
        Some(code) if !code.is_empty() => Ok(CallbackResult::Code(code.clone())),
        _ => Err(
            "No `code` in that address — paste the full address the browser ended up on.".into(),
        ),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn code(r: std::result::Result<CallbackResult, String>) -> String {
        match r {
            Ok(CallbackResult::Code(c)) => c,
            Ok(CallbackResult::Error(e)) => panic!("oauth error: {e}"),
            Err(e) => panic!("rejected: {e}"),
        }
    }

    #[test]
    fn accepts_the_full_callback_address() {
        let pasted = "http://127.0.0.1:43967/callback?code=DLObtVIH_Bvl&state=S1\n";
        assert_eq!(code(parse_pasted(pasted, "S1")), "DLObtVIH_Bvl");
    }

    #[test]
    fn accepts_just_the_query_and_quotes() {
        assert_eq!(code(parse_pasted("'code=abc&state=S1'", "S1")), "abc");
    }

    #[test]
    fn accepts_a_bare_code() {
        assert_eq!(
            code(parse_pasted("  DLObtVIH_BvlHcLzngjM  ", "S1")),
            "DLObtVIH_BvlHcLzngjM"
        );
    }

    #[test]
    fn rejects_a_callback_from_another_attempt() {
        let err = parse_pasted("http://127.0.0.1:1/callback?code=x&state=OTHER", "S1")
            .err()
            .unwrap();
        assert!(err.contains("different login attempt"), "{err}");
    }

    #[test]
    fn explains_when_the_login_url_itself_is_pasted() {
        let err = parse_pasted(
            "https://overpay.example/oauth/authorize?response_type=code&code_challenge=abc&state=S1",
            "S1",
        )
        .err()
        .unwrap();
        assert!(err.contains("login address itself"), "{err}");
    }

    #[test]
    fn surfaces_an_oauth_denial() {
        match parse_pasted(
            "http://127.0.0.1:1/callback?error=access_denied&state=S1",
            "S1",
        ) {
            Ok(CallbackResult::Error(e)) => assert!(e.contains("access_denied")),
            _ => panic!("expected an oauth error"),
        }
    }

    #[test]
    fn rejects_junk() {
        assert!(parse_pasted("hello", "S1").is_err());
    }
}
