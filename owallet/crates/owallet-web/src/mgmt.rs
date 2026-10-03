//! `/_mgmt/*` — the JSON API standing in for the CLI verbs norm's bootstrap
//! runs natively. Every handler reuses the library function the CLI uses
//! (`Database::init`/`unlock`, `PreparedWallet` + `Database::store_wallet`,
//! `Database::mint_provider_key`, `OverpayClient::{begin,finish}_pkce_login`,
//! `OverpayClient::register_buyer`). Errors: see [`MgmtError`].

use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use owallet_crypto::{derive_from_stored_seed, WordCount};
use owallet_db::{parse_budget_usd, Database, MintProviderKeyError, PreparedWallet};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{App, MgmtError};

type MgmtResult = Result<Json<Value>, MgmtError>;

pub(crate) fn router(app: App) -> Router<App> {
    let _ = app;
    Router::new()
        .route("/status", get(status))
        .route("/init", post(init))
        .route("/unlock", post(unlock))
        .route("/generate", post(generate))
        .route("/import", post(import))
        .route("/select", post(select))
        .route("/provider-key/create", post(provider_key_create))
        .route("/overpay/register", post(overpay_register))
        .route("/overpay/pkce/start", post(pkce_start))
        .route("/overpay/pkce/finish", post(pkce_finish))
        .route("/credits", get(credits))
        .route("/demo-credits", get(demo_credits).post(claim_demo_credits))
}

/// Parse a JSON body; an empty body reads as `{}`.
fn body<T: DeserializeOwned>(bytes: &Bytes) -> Result<T, MgmtError> {
    let raw: &[u8] = if bytes.iter().all(u8::is_ascii_whitespace) {
        b"{}"
    } else {
        bytes
    };
    serde_json::from_slice(raw)
        .map_err(|e| MgmtError::bad_request("bad_request", format!("request body: {e}")))
}

fn lock(db: &Mutex<Database>) -> Result<std::sync::MutexGuard<'_, Database>, MgmtError> {
    db.lock()
        .map_err(|_| MgmtError::internal("database mutex poisoned"))
}

/// The open database, or `not_initialized`.
fn opened(app: &App) -> Result<Arc<Mutex<Database>>, MgmtError> {
    app.db()?.ok_or_else(MgmtError::not_initialized)
}

/// The selected wallet of an unlocked database: `(npub, stored seed)`.
fn selected_wallet(db: &Database) -> Result<(String, String), MgmtError> {
    if !db.is_unlocked() {
        return Err(MgmtError::locked());
    }
    let npub = db
        .read_default_npub()
        .map_err(MgmtError::db)?
        .ok_or_else(MgmtError::no_wallet)?;
    let seed = db
        .read_seed(&npub)
        .map_err(MgmtError::db)?
        .ok_or_else(MgmtError::no_wallet)?;
    Ok((npub, seed))
}

// ---- status ----

/// `GET /_mgmt/status`.
async fn status(State(app): State<App>) -> MgmtResult {
    Ok(Json(status_value(&app)?))
}

fn status_value(app: &App) -> Result<Value, MgmtError> {
    let (initialized, unlocked, npub, linked) = match app.db()? {
        None => (false, false, None, false),
        Some(db) => {
            let db = lock(&db)?;
            let npub = db.read_default_npub().map_err(MgmtError::db)?;
            let host = app.overpay().host_key();
            let linked = match (&npub, db.is_unlocked()) {
                (Some(n), true) => matches!(db.read_token(n, &host), Ok(Some(_))),
                _ => false,
            };
            (true, db.is_unlocked(), npub, linked)
        }
    };
    let endpoints = app.endpoints();
    Ok(json!({
        "version": owallet_mcp::VERSION,
        "initialized": initialized,
        "unlocked": unlocked,
        "wallet": npub.map(|n| json!({ "npub": n })),
        "overpay_linked": linked,
        "env": endpoints.env,
        "rails_url": owallet_overpay::host_key(&endpoints.rails_url),
    }))
}

// ---- init / unlock ----

#[derive(Deserialize)]
struct PasswordBody {
    password: String,
}

/// `POST /_mgmt/init {password}` — create the encrypted database (and leave
/// it unlocked). The CLI's `owallet init`.
async fn init(State(app): State<App>, raw: Bytes) -> MgmtResult {
    let PasswordBody { password } = body(&raw)?;
    if password.is_empty() {
        return Err(MgmtError::bad_request(
            "bad_request",
            "password must not be empty",
        ));
    }
    if app.db()?.is_some() {
        return Err(MgmtError::conflict(
            "already_initialized",
            "a wallet database already exists — POST /_mgmt/unlock instead",
        ));
    }
    let db = Database::init(app.db_path(), &password).map_err(MgmtError::db)?;
    app.install(db)?;
    Ok(Json(status_value(&app)?))
}

/// `POST /_mgmt/unlock {password}`.
async fn unlock(State(app): State<App>, raw: Bytes) -> MgmtResult {
    let PasswordBody { password } = body(&raw)?;
    let db = opened(&app)?;
    {
        let mut db = lock(&db)?;
        if !db.unlock(&password).map_err(MgmtError::db)? {
            return Err(MgmtError::new(
                StatusCode::UNAUTHORIZED,
                "bad_password",
                "wrong password",
            ));
        }
    }
    Ok(Json(status_value(&app)?))
}

// ---- generate / import ----

#[derive(Deserialize, Default)]
struct GenerateBody {
    /// Per-wallet (dashboard) password; optional in the browser.
    #[serde(default)]
    wallet_password: Option<String>,
    /// 12 (default) or 24.
    #[serde(default)]
    words: Option<u8>,
}

/// `POST /_mgmt/generate {wallet_password?, words?}` → `{npub}`. Selects the
/// new wallet. The mnemonic never leaves the database.
async fn generate(State(app): State<App>, raw: Bytes) -> MgmtResult {
    let req: GenerateBody = body(&raw)?;
    let words = match req.words.unwrap_or(12) {
        12 => WordCount::Twelve,
        24 => WordCount::TwentyFour,
        n => {
            return Err(MgmtError::bad_request(
                "bad_request",
                format!("words must be 12 or 24, got {n}"),
            ))
        }
    };
    let wallet = PreparedWallet::generate(words)
        .map_err(|e| MgmtError::internal(format!("wallet derivation: {e}")))?;
    store_and_select(&app, &wallet, req.wallet_password.as_deref())
}

#[derive(Deserialize)]
struct ImportBody {
    /// A BIP-39 phrase, or a hex private key.
    mnemonic: String,
    #[serde(default)]
    wallet_password: Option<String>,
}

/// `POST /_mgmt/import {mnemonic, wallet_password?}` → `{npub}`. Selects the
/// imported wallet.
async fn import(State(app): State<App>, raw: Bytes) -> MgmtResult {
    let req: ImportBody = body(&raw)?;
    let wallet = PreparedWallet::from_secret(&req.mnemonic)
        .map_err(|e| MgmtError::bad_request("bad_mnemonic", e.to_string()))?;
    store_and_select(&app, &wallet, req.wallet_password.as_deref())
}

fn store_and_select(
    app: &App,
    wallet: &PreparedWallet,
    wallet_password: Option<&str>,
) -> MgmtResult {
    let db = opened(app)?;
    let db = lock(&db)?;
    if !db.is_unlocked() {
        return Err(MgmtError::locked());
    }
    // Like the CLI: a wallet password is only ever set once.
    let pw = match wallet_password.filter(|p| !p.is_empty()) {
        Some(p)
            if !db
                .has_wallet_password(&wallet.npub)
                .map_err(MgmtError::db)? =>
        {
            Some(p)
        }
        _ => None,
    };
    db.store_wallet(wallet, pw).map_err(MgmtError::db)?;
    db.write_default_npub(&wallet.npub).map_err(MgmtError::db)?;
    Ok(Json(json!({ "npub": wallet.npub })))
}

#[derive(Deserialize)]
struct SelectBody {
    npub: String,
}

/// `POST /_mgmt/select {npub}` → `{npub}`: make a stored wallet the
/// default (the CLI's `owallet select`). Decrypts its seed to prove it is
/// there and readable.
async fn select(State(app): State<App>, raw: Bytes) -> MgmtResult {
    let SelectBody { npub } = body(&raw)?;
    let db = opened(&app)?;
    let db = lock(&db)?;
    if !db.is_unlocked() {
        return Err(MgmtError::locked());
    }
    if db.read_seed(&npub).map_err(MgmtError::db)?.is_none() {
        return Err(MgmtError::new(
            StatusCode::NOT_FOUND,
            "unknown_wallet",
            format!("no wallet {npub} in this database"),
        ));
    }
    db.write_default_npub(&npub).map_err(MgmtError::db)?;
    Ok(Json(json!({ "npub": npub })))
}

// ---- provider keys ----

#[derive(Deserialize)]
struct ProviderKeyBody {
    #[serde(default = "default_label")]
    label: String,
    #[serde(default)]
    spend: bool,
    /// Daily budget in USD: a number, a string (`"5"`, `"$5.00"`), or
    /// null/absent for no limit.
    #[serde(default)]
    budget_usd: Option<Value>,
}

fn default_label() -> String {
    "norm".into()
}

/// `POST /_mgmt/provider-key/create {label, spend, budget_usd}` → exactly
/// what `owallet provider-key create --json` prints:
/// `{key, id, npub, label, scopes, daily_budget_usd_cents}`.
async fn provider_key_create(State(app): State<App>, raw: Bytes) -> MgmtResult {
    let req: ProviderKeyBody = body(&raw)?;
    let budget_text = match &req.budget_usd {
        None | Some(Value::Null) => None,
        Some(Value::Number(n)) => Some(n.to_string()),
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => {
            return Err(MgmtError::bad_request(
                "invalid_budget",
                "budget_usd must be a number, a string, or null",
            ))
        }
    };
    let cents = parse_budget_usd(budget_text.as_deref())
        .map_err(|e| MgmtError::bad_request("invalid_budget", e))?;
    let db = opened(&app)?;
    let db = lock(&db)?;
    if !db.is_unlocked() {
        return Err(MgmtError::locked());
    }
    let minted = db
        .mint_provider_key(None, &req.label, req.spend, cents)
        .map_err(|e| match e {
            MintProviderKeyError::NoDefaultWallet | MintProviderKeyError::UnknownWallet(_) => {
                MgmtError::no_wallet()
            }
            MintProviderKeyError::Db(e) => MgmtError::db(e),
        })?;
    Ok(Json(minted.to_json()))
}

// ---- Overpay link ----

/// File an Overpay bearer for the selected wallet, under the same key
/// `owallet authorize` uses, and cache the account's username.
async fn store_link(
    app: &App,
    db: &Mutex<Database>,
    npub: &str,
    token: &str,
    token_name: &str,
) -> Result<Value, MgmtError> {
    lock(db)?
        .write_token(npub, &app.overpay().host_key(), token, token_name)
        .map_err(MgmtError::db)?;
    // Best effort, like the CLI: confirm the link and remember the name.
    let account = app
        .overpay()
        .account(owallet_overpay::Auth::Bearer(token))
        .await
        .ok();
    if let Some(u) = account.as_ref().and_then(|a| a.username.as_deref()) {
        let _ = lock(db)?.cache_wallet_username(npub, u);
    }
    Ok(json!({
        "linked": true,
        "npub": npub,
        "username": account.as_ref().and_then(|a| a.username.clone()),
        "account_number": account.and_then(|a| a.account_number),
    }))
}

/// `POST /_mgmt/overpay/register` — zero-click sign-up: NIP-98-signed
/// `POST /api/v1/buyer/register` with the wallet key, then the returned API
/// token is stored like a browser-login bearer.
async fn overpay_register(State(app): State<App>, raw: Bytes) -> MgmtResult {
    let _: Value = body(&raw)?;
    let db = opened(&app)?;
    let (npub, seed) = selected_wallet(&*lock(&db)?)?;
    let sk = derive_from_stored_seed(&seed)
        .map_err(|e| MgmtError::internal(format!("wallet key: {e}")))?;
    drop(seed);
    let reg = app
        .overpay()
        .register_buyer(&sk, Some("owallet-web"))
        .await
        .map_err(MgmtError::overpay)?;
    let mut out = store_link(&app, &db, &npub, &reg.token, "overpay-nip98").await?;
    if out["account_number"].is_null() {
        out["account_number"] = json!(reg.account_number);
    }
    Ok(Json(out))
}

#[derive(Deserialize)]
struct PkceStartBody {
    redirect_uri: String,
}

/// `POST /_mgmt/overpay/pkce/start {redirect_uri}` → `{authorize_url, state}`.
/// The page opens `authorize_url` (a popup); Overpay redirects to
/// `redirect_uri?code=…&state=…`.
async fn pkce_start(State(app): State<App>, raw: Bytes) -> MgmtResult {
    let req: PkceStartBody = body(&raw)?;
    let login = app
        .overpay()
        .begin_pkce_login("owallet-web", &req.redirect_uri, "wallet")
        .await
        .map_err(MgmtError::overpay)?;
    let out = json!({
        "authorize_url": login.authorize_url.as_str(),
        "state": login.pkce.state,
    });
    app.logins().insert(login.pkce.state.clone(), login);
    Ok(Json(out))
}

#[derive(Deserialize)]
struct PkceFinishBody {
    code: String,
    state: String,
}

/// `POST /_mgmt/overpay/pkce/finish {code, state}` → `{linked, npub,
/// username, account_number}`.
async fn pkce_finish(State(app): State<App>, raw: Bytes) -> MgmtResult {
    let req: PkceFinishBody = body(&raw)?;
    let db = opened(&app)?;
    let (npub, _seed) = selected_wallet(&*lock(&db)?)?;
    let login = app.logins().remove(&req.state).ok_or_else(|| {
        MgmtError::bad_request(
            "unknown_state",
            "no login in progress for that state — start again",
        )
    })?;
    let token = app
        .overpay()
        .finish_pkce_login(&login, &req.code)
        .await
        .map_err(MgmtError::overpay)?;
    Ok(Json(
        store_link(&app, &db, &npub, &token.access_token, "overpay-oauth").await?,
    ))
}

// ---- credits ----

/// `GET /_mgmt/credits` — the wallet's merchant-credit balances, Overpay's
/// `GET /api/v1/merchant_credits` body verbatim (bearer, else NIP-98).
async fn credits(State(app): State<App>) -> MgmtResult {
    let db = opened(&app)?;
    selected_wallet(&*lock(&db)?)?;
    let (_, auth) = app
        .mcp_state(db)
        .resolve_owned_auth()
        .map_err(|e| MgmtError::internal(e.to_string()))?;
    let value = app
        .overpay()
        .list_merchant_credits_value(auth.as_auth())
        .await
        .map_err(MgmtError::overpay)?;
    Ok(Json(value))
}

// ---- demo credits ----

/// `GET /_mgmt/demo-credits` — whether this Overpay offers one-time demo
/// credits and whether the wallet's account got them: Overpay's
/// `{data: {enabled, amount_cents, granted}}` verbatim. An Overpay that
/// predates the endpoint (404) reads as "not offered".
async fn demo_credits(State(app): State<App>) -> MgmtResult {
    let db = opened(&app)?;
    selected_wallet(&*lock(&db)?)?;
    let (_, auth) = app
        .mcp_state(db)
        .resolve_owned_auth()
        .map_err(|e| MgmtError::internal(e.to_string()))?;
    match app.overpay().demo_credits_value(auth.as_auth()).await {
        Ok(value) => Ok(Json(value)),
        Err(owallet_overpay::OverpayError::HttpStatus { status: 404, .. }) => Ok(Json(json!({
            "data": { "enabled": false, "amount_cents": 0, "granted": false }
        }))),
        Err(e) => Err(MgmtError::overpay(e)),
    }
}

/// `POST /_mgmt/demo-credits` — claim them: Overpay's
/// `{data: {granted_cents, balance_cents}}` verbatim. Overpay's refusals keep
/// their status and `code` (404 `demo_credits_disabled`, 409
/// `already_granted`, 429 `demo_credits_exhausted`, 503 `not_configured`).
async fn claim_demo_credits(State(app): State<App>) -> MgmtResult {
    let db = opened(&app)?;
    selected_wallet(&*lock(&db)?)?;
    let (_, auth) = app
        .mcp_state(db)
        .resolve_owned_auth()
        .map_err(|e| MgmtError::internal(e.to_string()))?;
    match app.overpay().claim_demo_credits_value(auth.as_auth()).await {
        Ok(value) => Ok(Json(value)),
        Err(owallet_overpay::OverpayError::HttpStatus { status, body }) => {
            Err(demo_credit_refusal(status, &body))
        }
        Err(e) => Err(MgmtError::overpay(e)),
    }
}

/// Overpay's demo-credit refusal as a `/_mgmt` error with the same status and
/// code; anything unrecognised stays a generic `overpay_error`.
fn demo_credit_refusal(status: u16, body: &str) -> MgmtError {
    let parsed: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let message = parsed
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("Overpay refused the demo credits")
        .to_string();
    let code = match parsed.get("code").and_then(Value::as_str) {
        Some("demo_credits_disabled") => "demo_credits_disabled",
        Some("already_granted") => "already_granted",
        Some("demo_credits_exhausted") => "demo_credits_exhausted",
        Some("not_configured") => "not_configured",
        _ => {
            return MgmtError::overpay(owallet_overpay::OverpayError::HttpStatus {
                status,
                body: body.to_string(),
            })
        }
    };
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    MgmtError::new(status, code, message)
}

#[cfg(test)]
mod demo_credit_tests {
    use super::*;

    #[test]
    fn refusals_keep_overpay_status_and_code() {
        let e = demo_credit_refusal(
            409,
            r#"{"error":"already got them","code":"already_granted"}"#,
        );
        assert_eq!(e.status, StatusCode::CONFLICT);
        assert_eq!(e.code, "already_granted");
        assert_eq!(e.message, "already got them");
        let e = demo_credit_refusal(
            429,
            r#"{"error":"used up","code":"demo_credits_exhausted"}"#,
        );
        assert_eq!(e.status, StatusCode::TOO_MANY_REQUESTS);
    }

    #[test]
    fn unknown_refusals_are_generic_overpay_errors() {
        let e = demo_credit_refusal(500, "<html>boom</html>");
        assert_eq!(e.status, StatusCode::BAD_GATEWAY);
        assert_eq!(e.code, "overpay_error");
    }
}
