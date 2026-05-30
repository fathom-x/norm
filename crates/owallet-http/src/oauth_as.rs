//! Local OAuth 2.0 Authorization Server.
//!
//! Ports `wallet_mcp/oauth.py:44-161`. The local AS lets MCP clients
//! (Claude Code, etc.) authenticate against this binary itself via
//! standard OAuth — the `/consent` page asks the user to enter the wallet
//! password before issuing tokens.
//!
//! Routes (mounted under no prefix):
//! - `GET  /.well-known/oauth-authorization-server` — metadata document
//! - `POST /oauth/register` — dynamic client registration (RFC 7591)
//! - `GET  /oauth/authorize` — entry point; redirects to /consent
//! - `POST /oauth/token` — PKCE code exchange (S256 only)
//! - `GET  /consent` — wallet-password gated approval form
//! - `POST /consent` — verify password, issue auth code

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use askama::Template;
use axum::extract::{Form, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use dashmap::DashMap;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::AppError;
use crate::state::AppState;

const AUTH_CODE_TTL: Duration = Duration::from_secs(300);
const PENDING_TTL: Duration = Duration::from_secs(300);

#[derive(Clone)]
pub struct OAuthAsState {
    pub app: AppState,
    pub issuer_url: String,
    pub pending: PendingMap,
}

#[derive(Clone, Default)]
pub struct PendingMap(Arc<DashMap<String, Pending>>);

#[derive(Debug, Clone)]
struct Pending {
    client_id: String,
    redirect_uri: String,
    redirect_uri_provided_explicitly: bool,
    scopes: Vec<String>,
    state: Option<String>,
    code_challenge: String,
    expires_at: Instant,
}

pub fn router(state: OAuthAsState) -> Router {
    Router::new()
        .route(
            "/.well-known/oauth-authorization-server",
            get(well_known_metadata),
        )
        .route("/oauth/register", post(register_client))
        .route("/oauth/authorize", get(authorize))
        .route("/oauth/token", post(token_exchange))
        .route("/consent", get(consent_get).post(consent_post))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// /.well-known/oauth-authorization-server
// ---------------------------------------------------------------------------

async fn well_known_metadata(State(state): State<OAuthAsState>) -> Json<serde_json::Value> {
    let iss = state.issuer_url.trim_end_matches('/');
    Json(serde_json::json!({
        "issuer": iss,
        "authorization_endpoint": format!("{iss}/oauth/authorize"),
        "token_endpoint": format!("{iss}/oauth/token"),
        "registration_endpoint": format!("{iss}/oauth/register"),
        "scopes_supported": ["wallet"],
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
    }))
}

// ---------------------------------------------------------------------------
// POST /oauth/register
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RegisterRequest {
    #[serde(default)]
    client_name: Option<String>,
    #[serde(default)]
    redirect_uris: Vec<String>,
    #[serde(default)]
    grant_types: Vec<String>,
    #[serde(default)]
    response_types: Vec<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    token_endpoint_auth_method: Option<String>,
}

#[derive(Debug, Serialize)]
struct RegisterResponse {
    client_id: String,
    redirect_uris: Vec<String>,
    grant_types: Vec<String>,
    response_types: Vec<String>,
    scope: String,
    token_endpoint_auth_method: String,
}

async fn register_client(
    State(state): State<OAuthAsState>,
    Json(req): Json<RegisterRequest>,
) -> Result<Json<RegisterResponse>, AppError> {
    let client_id = format!("client_{}", rand_url_safe(16));
    let grant_types = if req.grant_types.is_empty() {
        vec!["authorization_code".to_string()]
    } else {
        req.grant_types
    };
    let response_types = if req.response_types.is_empty() {
        vec!["code".to_string()]
    } else {
        req.response_types
    };
    let scope = req.scope.unwrap_or_else(|| "wallet".to_string());
    let token_endpoint_auth_method = req
        .token_endpoint_auth_method
        .unwrap_or_else(|| "none".to_string());

    let db = state
        .app
        .db
        .lock()
        .map_err(|e| AppError::Internal(format!("db mutex: {e}")))?;
    db.write_oauth_client(
        &client_id,
        None,
        &req.redirect_uris,
        &grant_types,
        Some(&scope),
        Some(&token_endpoint_auth_method),
    )?;
    drop(db);

    let _ = req.client_name;
    Ok(Json(RegisterResponse {
        client_id,
        redirect_uris: req.redirect_uris,
        grant_types,
        response_types,
        scope,
        token_endpoint_auth_method,
    }))
}

// ---------------------------------------------------------------------------
// GET /oauth/authorize
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct AuthorizeQuery {
    response_type: String,
    client_id: String,
    #[serde(default)]
    redirect_uri: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    state: Option<String>,
    code_challenge: String,
    #[serde(default)]
    code_challenge_method: Option<String>,
}

async fn authorize(
    State(state): State<OAuthAsState>,
    Query(q): Query<AuthorizeQuery>,
) -> Result<Response, AppError> {
    if q.response_type != "code" {
        return Err(AppError::BadInput(
            "only response_type=code supported".into(),
        ));
    }
    if q.code_challenge_method.as_deref().unwrap_or("S256") != "S256" {
        return Err(AppError::BadInput(
            "only S256 challenge method supported".into(),
        ));
    }

    let db = state
        .app
        .db
        .lock()
        .map_err(|e| AppError::Internal(format!("db mutex: {e}")))?;
    let client = db
        .read_oauth_client(&q.client_id)?
        .ok_or_else(|| AppError::BadInput(format!("unknown client_id '{}'", q.client_id)))?;
    drop(db);

    let (redirect_uri, redirect_uri_provided_explicitly) = match q.redirect_uri.as_deref() {
        Some(uri) => {
            if !client.redirect_uris.contains(&uri.to_string()) {
                return Err(AppError::BadInput(format!(
                    "redirect_uri '{uri}' not registered for this client"
                )));
            }
            (uri.to_string(), true)
        }
        None => match client.redirect_uris.first() {
            Some(uri) => (uri.clone(), false),
            None => {
                return Err(AppError::BadInput(
                    "client has no registered redirect_uri".into(),
                ))
            }
        },
    };

    let session_id = rand_url_safe(24);
    let scopes = q
        .scope
        .as_deref()
        .map(|s| s.split_whitespace().map(str::to_string).collect::<Vec<_>>())
        .unwrap_or_else(|| vec!["wallet".to_string()]);

    state.pending.0.insert(
        session_id.clone(),
        Pending {
            client_id: q.client_id,
            redirect_uri,
            redirect_uri_provided_explicitly,
            scopes,
            state: q.state,
            code_challenge: q.code_challenge,
            expires_at: Instant::now() + PENDING_TTL,
        },
    );

    Ok(Redirect::to(&format!("/consent?session={session_id}")).into_response())
}

// ---------------------------------------------------------------------------
// GET|POST /consent
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ConsentQuery {
    session: String,
}

#[derive(Template)]
#[template(path = "consent.html")]
struct ConsentTemplate {
    session: String,
    wallets: Vec<(String, String)>, // (npub, label)
    default_npub: Option<String>,
    error: Option<String>,
}

async fn consent_get(
    State(state): State<OAuthAsState>,
    Query(q): Query<ConsentQuery>,
) -> Result<Response, AppError> {
    if !is_pending_alive(&state, &q.session) {
        return Ok((StatusCode::BAD_REQUEST, Html("<h2>Session expired.</h2>")).into_response());
    }
    Ok(Html(render_consent(&state, &q.session, None)?).into_response())
}

#[derive(Debug, Deserialize)]
struct ConsentForm {
    session: String,
    #[serde(default)]
    npub: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    action: String,
}

async fn consent_post(
    State(state): State<OAuthAsState>,
    Form(form): Form<ConsentForm>,
) -> Result<Response, AppError> {
    let Some(mut pending) = state
        .pending
        .0
        .get(&form.session)
        .map(|p| p.value().clone())
    else {
        return Ok((StatusCode::BAD_REQUEST, Html("<h2>Session expired.</h2>")).into_response());
    };
    if Instant::now() >= pending.expires_at {
        state.pending.0.remove(&form.session);
        return Ok((StatusCode::BAD_REQUEST, Html("<h2>Session expired.</h2>")).into_response());
    }

    if form.action != "approve" {
        state.pending.0.remove(&form.session);
        return Ok(Redirect::to(&construct_redirect(
            &pending.redirect_uri,
            None,
            Some("access_denied"),
            pending.state.as_deref(),
        ))
        .into_response());
    }

    // Pick the npub to bind. Fall back to default if the user didn't pick.
    let chosen_npub = {
        let db = state
            .app
            .db
            .lock()
            .map_err(|e| AppError::Internal(format!("db mutex: {e}")))?;
        let wallets = db.list_wallets()?;
        let known: std::collections::HashSet<_> = wallets.iter().map(|w| w.npub.clone()).collect();
        let candidate = if known.contains(&form.npub) {
            Some(form.npub.clone())
        } else {
            db.read_default_npub()?
                .or_else(|| wallets.first().map(|w| w.npub.clone()))
        };
        candidate
    };

    let Some(npub) = chosen_npub else {
        return Ok(Html(render_consent(
            &state,
            &form.session,
            Some("No wallets to authorize."),
        )?)
        .into_response());
    };

    // Verify the wallet password.
    let ok = {
        let db = state
            .app
            .db
            .lock()
            .map_err(|e| AppError::Internal(format!("db mutex: {e}")))?;
        db.verify_wallet_password(&npub, &form.password)?
    };
    if !ok {
        return Ok(Html(render_consent(
            &state,
            &form.session,
            Some("Incorrect wallet password."),
        )?)
        .into_response());
    }

    // Issue an authorization code.
    let code = rand_url_safe(20);
    let expires_at = unix_now_f64() + AUTH_CODE_TTL.as_secs() as f64;
    {
        let db = state
            .app
            .db
            .lock()
            .map_err(|e| AppError::Internal(format!("db mutex: {e}")))?;
        db.write_auth_code(
            &code,
            &pending.client_id,
            &pending.scopes,
            &pending.code_challenge,
            &pending.redirect_uri,
            pending.redirect_uri_provided_explicitly,
            expires_at,
            Some(&npub),
        )?;
    }

    let redirect_state = pending.state.take();
    state.pending.0.remove(&form.session);
    Ok(Redirect::to(&construct_redirect(
        &pending.redirect_uri,
        Some(&code),
        None,
        redirect_state.as_deref(),
    ))
    .into_response())
}

// ---------------------------------------------------------------------------
// POST /oauth/token
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct TokenForm {
    grant_type: String,
    code: String,
    #[serde(default)]
    redirect_uri: Option<String>,
    client_id: String,
    code_verifier: String,
}

#[derive(Debug, Serialize)]
struct TokenResponse {
    access_token: String,
    token_type: &'static str,
    scope: String,
}

async fn token_exchange(
    State(state): State<OAuthAsState>,
    Form(form): Form<TokenForm>,
) -> Result<Response, AppError> {
    if form.grant_type != "authorization_code" {
        return Err(AppError::BadInput(format!(
            "unsupported grant_type '{}'",
            form.grant_type
        )));
    }

    let row = {
        let db = state
            .app
            .db
            .lock()
            .map_err(|e| AppError::Internal(format!("db mutex: {e}")))?;
        let row = db
            .read_auth_code(&form.code)?
            .ok_or_else(|| AppError::BadInput("invalid code".into()))?;
        // Expire stale codes.
        if row.expires_at < unix_now_f64() {
            db.delete_auth_code(&form.code)?;
            return Err(AppError::BadInput("authorization code expired".into()));
        }
        if row.client_id != form.client_id {
            return Err(AppError::BadInput("client_id mismatch".into()));
        }
        if row.redirect_uri_provided_explicitly {
            let expected = row.redirect_uri.clone();
            if form.redirect_uri.as_deref() != Some(expected.as_str()) {
                return Err(AppError::BadInput("redirect_uri mismatch".into()));
            }
        }
        db.delete_auth_code(&form.code)?;
        row
    };

    // PKCE S256: base64url(SHA256(verifier)) == challenge
    let mut hasher = Sha256::new();
    hasher.update(form.code_verifier.as_bytes());
    let derived = base64_url(hasher.finalize().as_slice());
    if derived != row.code_challenge {
        return Err(AppError::BadInput("PKCE verifier mismatch".into()));
    }

    let access_token = format!("at_{}", rand_url_safe(32));
    {
        let db = state
            .app
            .db
            .lock()
            .map_err(|e| AppError::Internal(format!("db mutex: {e}")))?;
        db.write_access_token(
            &access_token,
            &row.client_id,
            &row.scopes,
            None, // non-expiring; the in-memory copy is gone on restart
            row.npub.as_deref(),
        )?;
    }

    Ok(Json(TokenResponse {
        access_token,
        token_type: "Bearer",
        scope: row.scopes.join(" "),
    })
    .into_response())
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn is_pending_alive(state: &OAuthAsState, session: &str) -> bool {
    state
        .pending
        .0
        .get(session)
        .map(|p| p.value().expires_at > Instant::now())
        .unwrap_or(false)
}

fn render_consent(
    state: &OAuthAsState,
    session: &str,
    error: Option<&str>,
) -> Result<String, AppError> {
    let db = state
        .app
        .db
        .lock()
        .map_err(|e| AppError::Internal(format!("db mutex: {e}")))?;
    let wallets = db.list_wallets()?;
    let default = db.read_default_npub()?;
    let mut entries: Vec<(String, String)> = Vec::new();
    for w in &wallets {
        let label = if w.npub.len() > 20 {
            format!("{}…{}", &w.npub[..12], &w.npub[w.npub.len() - 6..])
        } else {
            w.npub.clone()
        };
        entries.push((w.npub.clone(), label));
    }
    let tpl = ConsentTemplate {
        session: session.to_string(),
        wallets: entries,
        default_npub: default,
        error: error.map(str::to_string),
    };
    Ok(tpl.render()?)
}

fn construct_redirect(
    base: &str,
    code: Option<&str>,
    error: Option<&str>,
    state: Option<&str>,
) -> String {
    let sep = if base.contains('?') { '&' } else { '?' };
    let mut pairs: HashMap<&str, &str> = HashMap::new();
    if let Some(c) = code {
        pairs.insert("code", c);
    }
    if let Some(e) = error {
        pairs.insert("error", e);
    }
    if let Some(s) = state {
        pairs.insert("state", s);
    }
    let q: String = pairs
        .into_iter()
        .map(|(k, v)| format!("{k}={}", urlencoding(v)))
        .collect::<Vec<_>>()
        .join("&");
    if q.is_empty() {
        base.to_string()
    } else {
        format!("{base}{sep}{q}")
    }
}

fn urlencoding(s: &str) -> String {
    // Cheap percent-encoding — only escape characters that actually matter.
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn rand_url_safe(n_bytes: usize) -> String {
    let mut buf = vec![0u8; n_bytes];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    base64_url(&buf)
}

fn base64_url(bytes: &[u8]) -> String {
    // base64url(NO_PAD). Tiny self-contained encoder so we don't pull the
    // base64 crate just for one call site.
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity((bytes.len() * 4) / 3 + 4);
    let mut i = 0;
    while i + 3 <= bytes.len() {
        let n = ((bytes[i] as u32) << 16) | ((bytes[i + 1] as u32) << 8) | (bytes[i + 2] as u32);
        out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
        out.push(ALPHABET[((n >> 6) & 0x3f) as usize] as char);
        out.push(ALPHABET[(n & 0x3f) as usize] as char);
        i += 3;
    }
    let rem = bytes.len() - i;
    if rem == 1 {
        let n = (bytes[i] as u32) << 16;
        out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
    } else if rem == 2 {
        let n = ((bytes[i] as u32) << 16) | ((bytes[i + 1] as u32) << 8);
        out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
        out.push(ALPHABET[((n >> 6) & 0x3f) as usize] as char);
    }
    out
}

fn unix_now_f64() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Look up `bearer` in the local AS's access_tokens table and return the
/// wallet npub it was issued to (if any). Used by the MCP transport to
/// resolve `Authorization: Bearer …`.
pub fn lookup_token(app: &AppState, bearer: &str) -> owallet_mcp::transport::AuthResult {
    let db = match app.db.lock() {
        Ok(g) => g,
        Err(_) => return owallet_mcp::transport::AuthResult::Invalid,
    };
    match db.read_access_token(bearer) {
        Ok(Some(row)) => {
            if let Some(npub) = row.npub {
                owallet_mcp::transport::AuthResult::Wallet(npub)
            } else {
                owallet_mcp::transport::AuthResult::Anonymous
            }
        }
        _ => owallet_mcp::transport::AuthResult::Invalid,
    }
}
