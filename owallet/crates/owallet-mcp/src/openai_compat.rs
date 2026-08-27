//! OpenAI-compatible chat completions bridge.
//!
//! The localhost surface a client like OpenCode configures as a custom
//! "provider" (fathom-x/overpay#381): `GET /v1/models` and
//! `POST /v1/chat/completions`, both matching OpenAI's own wire shapes
//! closely enough that an unmodified OpenAI-compatible client works
//! against it. Every request is served by turning it into a paid Overpay
//! order — no Claude/OpenAI/etc API key needed, the wallet's own stored
//! Overpay auth pays instead (merchant credits; see [`place_and_pay`]).
//!
//! Hardcoded to the "OpenRouter Inference" listing (`open_router_provider`
//! bot, seller slug `openrouter-bot`) per the issue — the model catalog
//! IS that listing's `buyer_note_schema.properties.model.enum`, read live
//! rather than duplicated here, so the two can't drift (see
//! `Bots::OpenRouterProvider::OpenrouterInferenceListing::MODEL_OPTIONS`
//! in the Rails/Ruby source for the list itself). One extra model id is
//! always accepted on top of that live list — see [`DEFAULT_MODEL`].
//!
//! **Agentic tool-calling, run entirely server-side.** Every request
//! advertises a small tool set to the model: `run_python`, backed by the
//! "Run Python Code" listing (`code_executor` bot, seller slug `exec`),
//! plus the wallet tools (see below). When the model calls `run_python`,
//! this module executes it by placing and paying for a *second, real*
//! Overpay order, feeds stdout/stderr back to the model, and loops until a
//! turn produces no more tool calls. The HTTP caller never sees a
//! `tool_call` — just a normal chat completion that happens to have run
//! code along the way. Each iteration is a real, separately-paid order
//! (see [`MAX_TOOL_ITERATIONS`]); caller-supplied `tools` are not accepted
//! — this endpoint owns tool selection, since it is the one actually
//! executing them.
//!
//! **Wallet tools, privacy-projected.** The model can also read balances
//! (`get_balances`), browse the marketplace (`browse_marketplace` /
//! `get_listing`), browse recent orders (`list_orders`), confirm payments
//! (`get_order_status`), and — only when the provider key carries the
//! `spend` scope — run the full purchase loop order-scoped:
//! `create_order` (unpaid), `buy_credits` (top up a seller's merchant
//! credits), and `pay_order` (settle a pending order with those credits). These are
//! backed by the MCP tool handlers in [`crate::tools`], but their results
//! pass through **allowlist projections** ([`project_balances`] & co.)
//! before becoming tool messages: everything the model sees is appended to
//! `messages` and shipped inside the next turn's `buyer_note` to the
//! OpenRouter seller — a third party — so no on-chain data (txids,
//! tx hashes, addresses) may ever appear in a wallet tool result, in
//! either direction. Order ids, amounts, statuses, and spending limits
//! only. Raw-address sends (`send_usdc` / `send_zcash`) are deliberately
//! not offered here at all — they stay on the MCP/dashboard surfaces.
//! Spending is additionally bounded per request by [`SpendLedger`]
//! (default [`DEFAULT_SPEND_CAP_USD`], override via
//! [`SPEND_CAP_ENV`]) — [`MAX_TOOL_ITERATIONS`] bounds turns, but only a
//! dollar ceiling bounds what a turn can move — and, when the key carries
//! one, by the key's **persistent daily budget**
//! (`provider_keys.daily_budget_usd_cents`, a per-day allowance that
//! resets at midnight in the wallet's configured IANA timezone — UTC by
//! default): `buy_credits` reserves against it atomically in SQLite (so
//! parallel requests can't double-spend it, and the day rolls over inside
//! the same guarded UPDATE — no sweeper job) and
//! `pay_order` records redemptions after the fact; an exhausted budget
//! refuses further spending until midnight or until the wallet owner
//! raises it from the dashboard.
//!
//! Requests require a wallet-scoped provider API key issued by the dashboard.
//! It binds spending to that wallet; [`McpState::resolve_owned_auth`] then
//! authenticates it to Overpay for every order. The key's scopes gate the
//! spending tools: keys are chat-only unless minted with `spend`.

use std::convert::Infallible;
use std::time::{Duration, Instant};

use axum::extract::{Json, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;

use tokio::sync::OnceCell;

use crate::state::{McpState, OwnedAuth, ResolveAuthError};
use crate::tools::{new_output_since, partial_output, WAIT_TERMINAL_STATUSES};
use owallet_overpay::models::ListingFilters;
use owallet_overpay::OverpayError;

const OPENROUTER_SELLER_SLUG: &str = "openrouter-bot";
const OPENROUTER_LISTING_TITLE: &str = "OpenRouter Inference";
const PYTHON_SELLER_SLUG: &str = "exec";
const PYTHON_LISTING_TITLE: &str = "Run Python Code";
pub(crate) const RUN_PYTHON_TOOL_NAME: &str = "run_python";

// Model-facing wallet tool names. `buy_credits` (not the MCP tool's `buy`)
// because to the model it buys *merchant credits*, not products — the name
// should say what it does without the MCP catalog description around it.
const GET_BALANCES_TOOL: &str = "get_balances";
const BROWSE_MARKETPLACE_TOOL: &str = "browse_marketplace";
const GET_LISTING_TOOL: &str = "get_listing";
const LIST_ORDERS_TOOL: &str = "list_orders";
const GET_ORDER_STATUS_TOOL: &str = "get_order_status";
const CREATE_ORDER_TOOL: &str = "create_order";
const PAY_ORDER_TOOL: &str = "pay_order";
const BUY_CREDITS_TOOL: &str = "buy_credits";

/// Default per-request ceiling, in USD, on what the wallet spending tools
/// may move (cumulative across every tool call in one chat completion).
/// [`MAX_TOOL_ITERATIONS`] bounds how many turns a request gets; this
/// bounds the dollars a prompt-injected or confused model can spend within
/// them. Chat's own internal orders (the OpenRouter turn, `run_python`)
/// are not counted *here* — per request they're bounded by the iteration
/// cap — but they do count against the key's **daily budget**, which
/// bounds everything the key costs per day.
const DEFAULT_SPEND_CAP_USD: f64 = 20.0;
/// Environment variable overriding [`DEFAULT_SPEND_CAP_USD`].
const SPEND_CAP_ENV: &str = "OWALLET_V1_SPEND_CAP_USD";

/// A model id that always works, without needing a live catalog fetch to
/// validate it: `validate_request` accepts it unconditionally and
/// `GET /v1/models` always lists it, first. No OpenRouter model id ever
/// takes this shape (every real one is `vendor/model-name`), so it can't
/// collide. It's forwarded to the listing's `buyer_note.model` as-is —
/// `OpenrouterInferenceListing#coerce_model` already treats any string
/// outside its own `MODEL_OPTIONS` (this one included) as "use my own
/// default", the same as an unlisted or stale model id would be, and
/// resolves it to a real, concrete model id before it ever reaches
/// OpenRouter's API (which requires one). This is what lets
/// `owallet install --opencode-*` write a working provider entry even when
/// it couldn't reach a live server to fetch the real curated list — see
/// `commands::install::build_provider_entries` in the `owallet` crate.
const DEFAULT_MODEL: &str = "default";

/// Hard cap on OpenRouter turns per chat completion request. Was 4 when
/// the only tool was `run_python` and every iteration implied a second
/// paid order; most iterations are now cheap read-only wallet/marketplace
/// calls, and the full purchase loop (browse → get_listing → create_order
/// → pay_order → confirm) legitimately needs five or more. Each turn still
/// redeems credits for the chat order itself, so this remains a real bound
/// on the endpoint's own operating spend — the dollars the wallet tools
/// can *move* are bounded separately by [`SpendLedger`]. Reaching the cap
/// no longer fails the request outright: by then real orders may have been
/// created and paid, so one final turn runs with `tool_choice: "none"`,
/// forcing the model to report what it actually did.
const MAX_TOOL_ITERATIONS: u32 = 10;

/// How long a single order (an OpenRouter turn, or a run_python execution)
/// waits to finish before giving up. OpenAI's own API has no
/// server-advertised timeout (that's a client-side concern), but
/// *something* has to bound how long we hold the HTTP connection open for
/// a stuck order.
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
/// Poll cadence, matching `wait_for_order`'s own floor/default — see that
/// tool's docs for why 1s: the marketplace's own broadcast fan-out
/// (Solid Cable) polls at roughly the same granularity today.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_secs(1);
/// Poll cadence while a live cable subscription is delivering frames —
/// the poll is only a safety net then (terminal detection, resync), so
/// it backs off. Keep-alive comments ride the poll, and intermediary
/// idle timeouts are tens of seconds, so 5s stays comfortably safe.
pub(crate) const FALLBACK_POLL_INTERVAL: Duration = Duration::from_secs(5);
/// Env overrides for the timing knobs above and the WS toggle. Not wire
/// protocol — real OpenAI clients can't ask a server for different
/// timing — but ops-level tuning without a rebuild.
const POLL_MS_ENV: &str = "OWALLET_V1_POLL_MS";
const TIMEOUT_S_ENV: &str = "OWALLET_V1_TIMEOUT_S";
const FALLBACK_POLL_MS_ENV: &str = "OWALLET_V1_FALLBACK_POLL_MS";
/// `OWALLET_V1_WS=0` disables the cable subscription entirely, reverting
/// to pure polling. Default on: every WS failure already degrades to
/// exactly the polling behavior, so the toggle exists for diagnosis, not
/// safety.
const WS_ENV: &str = "OWALLET_V1_WS";

/// Axum state: the wallet's shared `McpState` plus this endpoint's own
/// request-timeout/poll-cadence config. Kept separate from `McpState`
/// itself (rather than adding fields there) since these constants are
/// specific to serving synchronous HTTP requests and don't belong on the
/// struct MCP tool calls share.
#[derive(Clone)]
struct Ctx {
    mcp: McpState,
    timeout: Duration,
    poll: Duration,
    /// Poll cadence while a live cable subscription is feeding frames —
    /// see [`FALLBACK_POLL_INTERVAL`].
    fallback_poll: Duration,
    /// Whether the streaming path tries the marketplace's WebSocket
    /// channel at all — see [`WS_ENV`].
    ws_enabled: bool,
    /// Whether the authenticated provider key carries the `spend` scope.
    /// `false` at construction; set per request from the key's stored
    /// scopes in [`chat_completions`].
    can_spend: bool,
    /// The authenticated key's row id — the handle the daily budget is
    /// accounted against. Every order the endpoint pays on the key's
    /// behalf (chat turns, `run_python`, and the wallet spending tools)
    /// records here. `None` at construction; set per request.
    key_id: Option<String>,
    /// Fallback per-request USD ceiling for the wallet spending tools —
    /// see [`SpendLedger`]. Construction-time (env override or default);
    /// a wallet-level dashboard setting takes precedence per request via
    /// [`effective_spend_cap`].
    spend_cap_usd: f64,
    /// Per-router cache of `provider_tool`-marked listings. On `Ctx`
    /// rather than a process-global so each serve env (and each test
    /// router) resolves its own marketplace's tools.
    listing_tools: Arc<OnceCell<Vec<ListingTool>>>,
}

pub fn router(state: McpState) -> Router {
    router_with_timing(state, REQUEST_TIMEOUT, POLL_INTERVAL)
}

/// Same as [`router`], but with the request timeout / poll cadence
/// overridable — lets tests exercise the timeout path without waiting on
/// the real 120s production ceiling. Not part of the wire protocol: real
/// OpenAI-compatible clients have no way to ask for a different value
/// (that's client-side in the real API too), so this is a construction-time
/// knob only.
fn router_with_timing(state: McpState, timeout: Duration, poll: Duration) -> Router {
    let cap = std::env::var(SPEND_CAP_ENV)
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v >= 0.0)
        .unwrap_or(DEFAULT_SPEND_CAP_USD);
    let timeout = env_u64(TIMEOUT_S_ENV)
        .map(Duration::from_secs)
        .unwrap_or(timeout);
    let poll = env_u64(POLL_MS_ENV)
        .map(Duration::from_millis)
        .unwrap_or(poll);
    let fallback_poll = env_u64(FALLBACK_POLL_MS_ENV)
        .map(Duration::from_millis)
        .unwrap_or(FALLBACK_POLL_INTERVAL)
        .max(poll);
    let ws_enabled = std::env::var(WS_ENV).map(|v| v != "0").unwrap_or(true);
    router_with_config_full(state, timeout, poll, cap, ws_enabled, fallback_poll)
}

fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
}

/// Innermost constructor: also fixes the spend cap, bypassing the env
/// read — tests use this so a parallel test can't race another's
/// process-global environment.
fn router_with_config_full(
    state: McpState,
    timeout: Duration,
    poll: Duration,
    cap: f64,
    ws_enabled: bool,
    fallback_poll: Duration,
) -> Router {
    let ctx = Ctx {
        mcp: state,
        timeout,
        poll,
        fallback_poll,
        ws_enabled,
        can_spend: false,
        key_id: None,
        spend_cap_usd: cap,
        listing_tools: Arc::new(OnceCell::new()),
    };
    Router::new()
        .route("/models", get(list_models))
        .route("/status", get(wallet_status))
        .route("/chat/completions", post(chat_completions))
        .with_state(ctx)
}

/// `GET /v1/status` — chain-free wallet status for norm's TUI sidebar
/// (fathom-x/norm#9): balances, merchant credits, and the calling key's
/// daily budget, stamped `as_of` in the wallet's timezone. Same
/// provider-key auth and the same allowlist projection as the
/// `get_balances` wallet tool, minus the per-request spend allowance —
/// that ledger only exists inside a chat request. Poll-friendly but not
/// free: the underlying account read hits the EVM RPC and Overpay live,
/// so callers should poll on the order of a minute, not a second.
async fn wallet_status(
    State(ctx): State<Ctx>,
    headers: HeaderMap,
) -> Result<Json<Value>, OpenAiError> {
    let (state, _can_spend, key_id) = authenticate_provider_key(&ctx.mcp, &headers)?;
    let out = crate::tools::dispatch(&state, "get_account_info", json!({}), None)
        .await
        .map_err(|e| OpenAiError::internal(format!("get_account_info: {e}")))?;
    let mut map = crate::projection::balances_map(&out.data);
    // Whether this wallet is linked to an Overpay account: the underlying
    // account read yields `account` when the live fetch succeeded and
    // `account_hint` (run `owallet authorize`, sign up, ...) when it
    // didn't. norm's sidebar turns `false` into its "log in to Overpay to
    // get started" line.
    map.insert(
        "overpay_connected".into(),
        Value::Bool(out.data.get("account").is_some()),
    );
    if let Some(key) = read_key(&state, key_id.as_deref()) {
        map.insert("key_budget".into(), key_budget_json(&key));
    }
    // The marketplace this wallet is pointed at (env-resolved, so norm's
    // sidebar links the right Overpay per staging/prod build without its
    // own copy of the URL table).
    map.insert(
        "overpay_url".into(),
        Value::String(
            state
                .overpay
                .base_url()
                .as_str()
                .trim_end_matches('/')
                .to_string(),
        ),
    );
    Ok(Json(stamp_as_of(&state, Value::Object(map))))
}

// ---------------------------------------------------------------------------
// Errors — OpenAI's `{error: {message, type, param, code}}` envelope, so an
// unmodified OpenAI-compatible client's error handling still works.
// ---------------------------------------------------------------------------

pub(crate) enum OpenAiError {
    InvalidRequest(String),
    Unauthorized(String),
    PaymentRequired(String),
    UpstreamFailure(String),
    Internal(String),
}

impl OpenAiError {
    fn internal(msg: impl Into<String>) -> Self {
        Self::Internal(msg.into())
    }

    fn status_and_type(&self) -> (StatusCode, &'static str) {
        match self {
            Self::InvalidRequest(_) => (StatusCode::BAD_REQUEST, "invalid_request_error"),
            Self::Unauthorized(_) => (StatusCode::UNAUTHORIZED, "authentication_error"),
            Self::PaymentRequired(_) => (StatusCode::PAYMENT_REQUIRED, "insufficient_quota"),
            Self::UpstreamFailure(_) => (StatusCode::BAD_GATEWAY, "api_error"),
            Self::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "api_error"),
        }
    }

    pub(crate) fn message(&self) -> &str {
        match self {
            Self::InvalidRequest(m)
            | Self::Unauthorized(m)
            | Self::PaymentRequired(m)
            | Self::UpstreamFailure(m)
            | Self::Internal(m) => m,
        }
    }
}

impl From<OverpayError> for OpenAiError {
    fn from(e: OverpayError) -> Self {
        Self::UpstreamFailure(e.to_string())
    }
}

impl From<ResolveAuthError> for OpenAiError {
    fn from(e: ResolveAuthError) -> Self {
        // Every variant here means "the wallet itself isn't ready to act
        // on Overpay's behalf" (no wallet selected, DB locked, no stored
        // auth) — an operator problem, not the caller's. `owallet
        // authorize` / `owallet select` are the fix either way.
        Self::Unauthorized(format!(
            "wallet is not ready to place Overpay orders: {e} (run `owallet authorize`)"
        ))
    }
}

impl IntoResponse for OpenAiError {
    fn into_response(self) -> Response {
        let (status, err_type) = self.status_and_type();
        let body = json!({
            "error": {
                "message": self.message(),
                "type": err_type,
                "param": Value::Null,
                "code": Value::Null,
            }
        });
        (status, Json(body)).into_response()
    }
}

// ---------------------------------------------------------------------------
// GET /v1/models
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct ModelObject {
    id: String,
    object: &'static str,
    created: i64,
    owned_by: &'static str,
}

#[derive(Serialize)]
struct ModelList {
    object: &'static str,
    data: Vec<ModelObject>,
}

async fn list_models(
    State(ctx): State<Ctx>,
    headers: HeaderMap,
) -> Result<Json<ModelList>, OpenAiError> {
    let (mcp, _can_spend, _spend_key_id) = authenticate_provider_key(&ctx.mcp, &headers)?;
    let mut models = resolve_models(&mcp).await?;
    // Always offered, first — see `DEFAULT_MODEL`'s doc comment.
    models.insert(0, DEFAULT_MODEL.to_string());
    Ok(Json(ModelList {
        object: "list",
        data: models
            .into_iter()
            .map(|id| ModelObject {
                id,
                object: "model",
                created: 0,
                owned_by: "overpay",
            })
            .collect(),
    }))
}

/// The curated model list, read live off the listing's own
/// `buyer_note_schema` rather than duplicated here — see the module doc.
async fn resolve_models(state: &McpState) -> Result<Vec<String>, OpenAiError> {
    let listing_id = resolve_openrouter_listing_id(state).await?;
    let listing = state.overpay.get_listing_value(&listing_id).await?;
    let inner = listing.get("data").unwrap_or(&listing);
    let models = inner
        .pointer("/buyer_note_schema/properties/model/enum")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Ok(models)
}

// Each id is derived from its bot's private key
// (`Overpay::Uuid.derive_listing_id`) and so differs per environment —
// neither can be a literal constant. Cached per marketplace on
// `McpState::listing_ids` (see `ListingIdCache` for why not a process
// global), with no invalidation: an id never changes for a given Overpay
// instance during a process's lifetime.
async fn resolve_openrouter_listing_id(state: &McpState) -> Result<String, OpenAiError> {
    resolve_listing_id_cached(
        state,
        OPENROUTER_SELLER_SLUG,
        OPENROUTER_LISTING_TITLE,
        &state.listing_ids.openrouter,
    )
    .await
}

async fn resolve_python_listing_id(state: &McpState) -> Result<String, OpenAiError> {
    resolve_listing_id_cached(
        state,
        PYTHON_SELLER_SLUG,
        PYTHON_LISTING_TITLE,
        &state.listing_ids.python,
    )
    .await
}

async fn resolve_listing_id_cached(
    state: &McpState,
    seller_slug: &str,
    title: &str,
    cache: &OnceCell<String>,
) -> Result<String, OpenAiError> {
    if let Some(id) = cache.get() {
        return Ok(id.clone());
    }

    let page = state
        .overpay
        .list_listings_value(&ListingFilters {
            seller_slug: Some(seller_slug.to_string()),
            limit: Some(20),
            ..Default::default()
        })
        .await?;

    let id = page
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|l| l.get("title").and_then(Value::as_str) == Some(title))
        .and_then(|l| l.get("id"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            OpenAiError::internal(format!(
                "could not find a '{title}' listing from seller '{seller_slug}' — is its bot registered?"
            ))
        })?
        .to_string();

    // Best-effort: a concurrent racer just resolves the same id again.
    let _ = cache.set(id.clone());
    Ok(id)
}

/// Cost marker appended to every listing-backed tool description: each
/// call places (and pays) a real marketplace order, and the roster itself
/// should say so instead of leaving clients to hardcode it prompt-side.
/// Priced from the listing's own fields (`price_usd` / `free`), so a
/// repriced listing needs no Rust change to stay honest (fathom-x/norm#17).
pub(crate) fn listing_cost_note(listing: &Value) -> String {
    let price = listing.get("price_usd").and_then(Value::as_str);
    if listing.get("free").and_then(Value::as_bool) == Some(true) || price == Some("Free") {
        return " Each call places a real marketplace order (this listing is currently free)."
            .to_string();
    }
    match price {
        Some(price) => format!(
            " Each call places a real marketplace order billed to the wallet (≈ {price} per call)."
        ),
        None => " Each call places a real marketplace order billed to the wallet.".to_string(),
    }
}

/// The `run_python` tool definition offered to the model on every turn.
/// `parameters` is the Python listing's own `buyer_note_schema` — already
/// valid JSON Schema shaped exactly like an OpenAI tool's `parameters`
/// field expects (`{code, stdin, requirements, requirements_lock}`), so
/// this can't drift from what the listing actually accepts the way a
/// hand-duplicated schema could.
pub(crate) async fn run_python_tool_def(state: &McpState) -> Result<Value, OpenAiError> {
    let listing_id = resolve_python_listing_id(state).await?;
    let listing = state.overpay.get_listing_value(&listing_id).await?;
    let inner = listing.get("data").unwrap_or(&listing);
    let parameters = inner
        .get("buyer_note_schema")
        .cloned()
        .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
    let description = inner
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("Run a Python 3.11 snippet in an isolated sandbox and return stdout, stderr, and exit code.");
    let description = format!("{description}{}", listing_cost_note(inner));

    Ok(json!({
        "type": "function",
        "function": {
            "name": RUN_PYTHON_TOOL_NAME,
            "description": description,
            "parameters": parameters,
        }
    }))
}

// ---------------------------------------------------------------------------
// Listing-backed tools — any listing marked `metadata.provider_tool`
// ---------------------------------------------------------------------------

/// A model-callable tool built from a `metadata.provider_tool`-marked
/// marketplace listing (the bot DSL's `provider_tool name: "..."`). The
/// listing supplies everything: the marker's `name` becomes the function
/// name, the listing description its description, and `buyer_note_schema`
/// its parameters. Executing a call places and pays a real order against
/// the listing and returns the delivered content — the generalization of
/// the hardcoded `run_python` path sketched in PR #383.
#[derive(Clone)]
pub(crate) struct ListingTool {
    pub(crate) name: String,
    listing_id: String,
    seller_slug: String,
    pub(crate) description: String,
    pub(crate) parameters: Value,
    /// True when the listing's schema was a bare (non-object) type —
    /// OpenAI tool parameters must be an object, so the schema is offered
    /// wrapped as `{input: <schema>}` and the execution unwraps
    /// `arguments.input` back into the buyer_note.
    wrapped: bool,
}

/// OpenAI function names: conservative charset, bounded length.
fn valid_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Marked listings, resolved once per router — the same tradeoff as the
/// listing-id caches: a listing marked after startup needs a restart. A
/// failed fetch is NOT cached; that request just runs without listing
/// tools and the next one retries.
async fn listing_tools(ctx: &Ctx) -> Vec<ListingTool> {
    if let Some(tools) = ctx.listing_tools.get() {
        return tools.clone();
    }
    match fetch_listing_tools(&ctx.mcp).await {
        Ok(tools) => {
            let _ = ctx.listing_tools.set(tools.clone());
            tools
        }
        Err(_) => Vec::new(),
    }
}

pub(crate) async fn fetch_listing_tools(state: &McpState) -> Result<Vec<ListingTool>, OpenAiError> {
    let page = state
        .overpay
        .list_listings_value(&ListingFilters {
            limit: Some(100),
            ..Default::default()
        })
        .await?;
    let mut tools: Vec<ListingTool> = Vec::new();
    for listing in page
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        // Rails exposes the marker as a curated top-level field
        // (`Listing#provider_tool`), like `delivery_eta` — the raw
        // metadata hash never rides the public JSON.
        let Some(name) = listing
            .pointer("/provider_tool/name")
            .and_then(Value::as_str)
        else {
            continue;
        };
        // The hardcoded run_python path stays authoritative for its name,
        // and nothing may shadow a wallet tool. First marked listing wins
        // a within-registry collision.
        if !valid_tool_name(name)
            || name == RUN_PYTHON_TOOL_NAME
            || is_wallet_tool(name)
            || tools.iter().any(|t| t.name == name)
        {
            continue;
        }
        let (Some(listing_id), Some(seller_slug)) = (
            listing.get("id").and_then(Value::as_str),
            listing.pointer("/seller/slug").and_then(Value::as_str),
        ) else {
            continue;
        };
        // The index deliberately omits the per-listing schemas and
        // truncates descriptions (browsing stays cheap), so fetch the
        // full listing for the function parameters — same as
        // `run_python_tool_def`. Propagating a failure means the registry
        // is not cached and the next request retries whole.
        let detail = state.overpay.get_listing_value(listing_id).await?;
        let inner = detail.get("data").unwrap_or(&detail);
        let schema = inner
            .get("buyer_note_schema")
            .cloned()
            .filter(|s| !s.is_null());
        let is_object_schema = schema
            .as_ref()
            .map(|s| s.get("type").and_then(Value::as_str) == Some("object"))
            .unwrap_or(false);
        let (parameters, wrapped) = match schema {
            Some(s) if is_object_schema => (s, false),
            Some(s) => (
                json!({
                    "type": "object",
                    "properties": {"input": s},
                    "required": ["input"],
                }),
                true,
            ),
            None => (json!({"type": "object", "properties": {}}), false),
        };
        tools.push(ListingTool {
            name: name.to_string(),
            listing_id: listing_id.to_string(),
            seller_slug: seller_slug.to_string(),
            description: format!(
                "{}{}",
                inner
                    .get("description")
                    .and_then(Value::as_str)
                    .or_else(|| listing.get("description").and_then(Value::as_str))
                    .unwrap_or("A marketplace listing offered as a callable tool."),
                // Price fields ride the index row too; prefer the detail
                // (`inner`) and fall back so either serialization works.
                if inner.get("price_usd").is_some() || inner.get("free").is_some() {
                    listing_cost_note(inner)
                } else {
                    listing_cost_note(listing)
                }
            ),
            parameters,
            wrapped,
        });
    }
    Ok(tools)
}

async fn listing_tool_named(ctx: &Ctx, name: &str) -> Option<ListingTool> {
    listing_tools(ctx)
        .await
        .into_iter()
        .find(|t| t.name == name)
}

fn listing_tool_def(tool: &ListingTool) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": tool.parameters,
        }
    })
}

/// Execute one listing-tool call: a real, separately-paid order against
/// the tool's listing, exactly like `run_python`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_listing_tool(
    state: &McpState,
    auth: &OwnedAuth,
    tool: &ListingTool,
    arguments: &Value,
    timeout: Duration,
    poll: Duration,
    key_id: Option<&str>,
    usage: &mut TurnUsage,
) -> Result<Value, OpenAiError> {
    let buyer_note = listing_tool_buyer_note(tool, arguments);
    let (order_id, redeemed_cents) = place_and_pay_order(
        state,
        auth,
        &tool.listing_id,
        &tool.seller_slug,
        &buyer_note,
        key_id,
    )
    .await?;
    let snap = wait_for_order_terminal(state, auth, &order_id, timeout, poll).await?;
    net_key_budget_from_delivery(state, key_id, &snap, redeemed_cents);
    usage.add_order(&snap, redeemed_cents);
    Ok(extract_listing_delivered(&order_id, &snap))
}

/// The buyer_note for a listing-tool call: the arguments verbatim, or —
/// for a wrapped bare-schema listing — the unwrapped `input` value.
fn listing_tool_buyer_note(tool: &ListingTool, arguments: &Value) -> Value {
    if tool.wrapped {
        arguments.get("input").cloned().unwrap_or(Value::Null)
    } else {
        arguments.clone()
    }
}

/// Project a listing-tool order's terminal snapshot into the tool result
/// fed back to the model. An allowlist, like every other model-facing
/// projection here: statuses, the delivered content (capped), its type,
/// and the download URL — never the raw order payload.
fn extract_listing_delivered(order_id: &str, snap: &Value) -> Value {
    let order = snap.get("data").unwrap_or(snap);
    let status = order
        .get("fulfillment_status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let mut out = serde_json::Map::new();
    out.insert("order_id".into(), json!(order_id));
    out.insert("fulfillment_status".into(), json!(status));
    if status != "delivered" {
        let reason = order
            .get("fulfillment_error")
            .and_then(Value::as_str)
            .unwrap_or("the seller did not deliver this order");
        out.insert("error".into(), json!(reason));
        return Value::Object(out);
    }
    if let Some(content) = order.get("delivered_content").and_then(Value::as_str) {
        if content.len() > DELIVERED_CONTENT_MODEL_CAP {
            let mut end = DELIVERED_CONTENT_MODEL_CAP;
            while !content.is_char_boundary(end) {
                end -= 1;
            }
            out.insert("delivered_content".into(), json!(&content[..end]));
            out.insert("delivered_content_truncated".into(), json!(true));
        } else {
            out.insert("delivered_content".into(), json!(content));
        }
    }
    for key in ["delivered_content_type", "delivered_content_url"] {
        if let Some(v) = order.get(key).filter(|v| !v.is_null()) {
            out.insert(key.into(), v.clone());
        }
    }
    Value::Object(out)
}

// ---------------------------------------------------------------------------
// Wallet tools — MCP handlers behind allowlist projections
// ---------------------------------------------------------------------------
//
// Backed by `crate::tools::dispatch`, but with their own model-facing
// definitions: the MCP catalog's descriptions promise txids and addresses
// ("Returns `{tx_hash}`…"), which a model here can never see — a definition
// that advertises them would just prompt the model to ask for data the
// projections strip. Argument schemas are defined here for the same reason
// (pay_order/buy take a subset of what MCP accepts). See the module doc for
// why the projections are allowlists, not field-stripping.

/// One model-facing wallet tool: its OpenAI function definition parts plus
/// whether it needs the provider key's `spend` scope.
struct WalletToolSpec {
    name: &'static str,
    description: &'static str,
    spend: bool,
    parameters: fn() -> Value,
}

const WALLET_TOOLS: &[WalletToolSpec] = &[
    WalletToolSpec {
        name: GET_BALANCES_TOOL,
        description: "Free — a read, no order is placed and nothing is billed. Check the \
                      wallet's balances: ETH / USDC / ZEC amounts, Overpay \
                      merchant-credit balances per seller, this request's remaining \
                      spending allowance, and this key's remaining daily budget \
                      (if one is set; it resets at midnight in the wallet's configured \
                      timezone). Results are point-in-time (see their as_of field) and \
                      change with every order — when asked for current balances or \
                      budget, call this again rather than reusing an earlier result \
                      from the conversation.",
        spend: false,
        parameters: || json!({"type": "object", "properties": {}, "additionalProperties": false}),
    },
    WalletToolSpec {
        name: BROWSE_MARKETPLACE_TOOL,
        description: "Free — a read, no order is placed and nothing is billed. Browse \
                      Overpay marketplace listings, with optional category / \
                      seller_slug filters and cursor paging. Call get_listing on the one \
                      you'd act on before create_order.",
        spend: false,
        parameters: || {
            json!({
                "type": "object",
                "properties": {
                    "category":    {"type": "string"},
                    "seller_slug": {"type": "string"},
                    "limit":       {"type": "integer", "minimum": 1, "maximum": 100},
                    "cursor":      {"type": "string", "description": "next_cursor from a previous page"},
                },
                "additionalProperties": false,
            })
        },
    },
    WalletToolSpec {
        name: GET_LISTING_TOOL,
        description: "Free — a read, no order is placed and nothing is billed. Fetch one \
                      listing's full description, price, and buyer_note_schema. \
                      Call this before create_order when the listing declares a structured \
                      buyer_note shape, so the note can be built to match.",
        spend: false,
        parameters: || {
            json!({
                "type": "object",
                "properties": {"listing_id": {"type": "string"}},
                "required": ["listing_id"],
                "additionalProperties": false,
            })
        },
    },
    WalletToolSpec {
        name: LIST_ORDERS_TOOL,
        description: "Free — a read, no order is placed and nothing is billed. \
                      List the wallet's recent orders, newest first — use this to find an \
                      order id when the user refers to an order without one (\"my pending \
                      order\"). Optional payment_status / fulfillment_status filters; pass \
                      the returned next_cursor to page further back. Results are \
                      point-in-time (see as_of) — re-call rather than reusing an earlier \
                      result when asked for the current list.",
        spend: false,
        parameters: || {
            json!({
                "type": "object",
                "properties": {
                    "payment_status":     {"type": "string", "description": "e.g. pending, paid"},
                    "fulfillment_status": {"type": "string", "description": "e.g. pending, awaiting_seller, delivered"},
                    "limit":              {"type": "integer", "minimum": 1, "maximum": 20},
                    "cursor":             {"type": "string", "description": "next_cursor from a previous page"},
                },
                "additionalProperties": false,
            })
        },
    },
    WalletToolSpec {
        name: GET_ORDER_STATUS_TOOL,
        description: "Free — a read, no order is placed and nothing is billed. \
                      Check an order's payment and fulfillment status by order id — use \
                      this to confirm a payment landed. Statuses are point-in-time (see \
                      as_of); re-call for the current state rather than reusing an \
                      earlier result. Once the order is delivered, the \
                      result includes the delivered content (the deliverable the buyer \
                      paid for), truncated if very large. URLs inside delivered content \
                      (images, downloads) are minted by the buyer's own marketplace host \
                      and may point at localhost in a dev setup — they are reachable from \
                      the user's machine, so present them as-is; do not declare them \
                      broken, and do not try to fetch them yourself.",
        spend: false,
        parameters: || {
            json!({
                "type": "object",
                "properties": {"order_id": {"type": "string"}},
                "required": ["order_id"],
                "additionalProperties": false,
            })
        },
    },
    WalletToolSpec {
        name: CREATE_ORDER_TOOL,
        description: "Free to call, and no money moves yet — the order it creates is \
                      unpaid until pay_order settles it with real funds. \
                      Create a pending order for a marketplace listing. buyer_note may be \
                      free-form text or a JSON object matching the listing's \
                      buyer_note_schema (pre-validated locally — violations come back \
                      spelled out). The order is created unpaid: settle it with pay_order. \
                      Returns the order id and status.",
        spend: true,
        parameters: || {
            json!({
                "type": "object",
                "properties": {
                    "listing_id": {"type": "string"},
                    "buyer_note": {"description": "Free-form string, or any JSON value matching the listing's buyer_note_schema."},
                },
                "required": ["listing_id"],
                "additionalProperties": false,
            })
        },
    },
    WalletToolSpec {
        name: PAY_ORDER_TOOL,
        description: "The call itself is not billed, but it moves real money: it spends \
                      the wallet's prepaid credits to settle the order. \
                      Pay a pending order with the wallet's prepaid merchant credits for \
                      that seller. The seller is resolved from the order automatically. \
                      Returns the redemption status and remaining credit balance; if \
                      credits are short, buy_credits first.",
        spend: true,
        parameters: || {
            json!({
                "type": "object",
                "properties": {
                    "order_id":    {"type": "string"},
                    "seller_slug": {"type": "string", "description": "Optional — resolved from the order when omitted."},
                },
                "required": ["order_id"],
                "additionalProperties": false,
            })
        },
    },
    WalletToolSpec {
        name: BUY_CREDITS_TOOL,
        description: "The call itself is not billed, but it moves real money from the \
                      wallet's on-chain funds. \
                      Top up merchant credits with a seller, paid from the wallet. \
                      Counts against this request's spending allowance and this key's \
                      daily budget, if one is set. Returns the credit-purchase \
                      order id and status.",
        spend: true,
        parameters: || {
            json!({
                "type": "object",
                "properties": {
                    "seller_slug": {"type": "string"},
                    "amount_usd":  {"type": "number"},
                },
                "required": ["seller_slug", "amount_usd"],
                "additionalProperties": false,
            })
        },
    },
];

fn is_wallet_tool(name: &str) -> bool {
    WALLET_TOOLS.iter().any(|t| t.name == name)
}

/// OpenAI function definitions for the wallet tools this key may use.
/// Spend-gated tools simply don't exist for a chat-only key — not
/// advertised, and (defense in depth) refused by [`execute_wallet_tool`]
/// if a model hallucinates one anyway.
fn wallet_tool_defs(can_spend: bool) -> Vec<Value> {
    WALLET_TOOLS
        .iter()
        .filter(|t| can_spend || !t.spend)
        .map(|t| {
            json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": (t.parameters)(),
                }
            })
        })
        .collect()
}

/// Every tool definition advertised to the model this turn: `run_python`
/// (listing-backed, resolved live), every `provider_tool`-marked listing,
/// plus the wallet tools the key's scopes allow.
async fn tool_defs(ctx: &Ctx, can_spend: bool) -> Result<Vec<Value>, OpenAiError> {
    let mut defs = vec![run_python_tool_def(&ctx.mcp).await?];
    for tool in listing_tools(ctx).await {
        defs.push(listing_tool_def(&tool));
    }
    defs.extend(wallet_tool_defs(can_spend));
    Ok(defs)
}

/// Cumulative per-request USD accounting for the wallet spending tools.
/// `buy_credits` reserves its amount up front (it's in the arguments);
/// `pay_order` records what the redemption actually applied after the
/// fact (partial redemptions make the amount unknowable up front).
struct SpendLedger {
    cap_usd: f64,
    spent_usd: f64,
}

impl SpendLedger {
    fn new(cap_usd: f64) -> Self {
        Self {
            cap_usd,
            spent_usd: 0.0,
        }
    }

    fn remaining_usd(&self) -> f64 {
        (self.cap_usd - self.spent_usd).max(0.0)
    }

    /// Reserve `amount_usd` against the cap, or say why not.
    fn try_spend(&mut self, amount_usd: f64) -> Result<(), String> {
        if !(amount_usd.is_finite() && amount_usd > 0.0) {
            return Err("amount_usd must be a positive number".into());
        }
        if self.spent_usd + amount_usd > self.cap_usd {
            return Err(format!(
                "spend cap exceeded: ${:.2} requested but only ${:.2} of this request's \
                 ${:.2} allowance remains",
                amount_usd,
                self.remaining_usd(),
                self.cap_usd
            ));
        }
        self.spent_usd += amount_usd;
        Ok(())
    }

    /// Return a reservation that turned out not to spend (payment refused
    /// before anything moved).
    fn release(&mut self, amount_usd: f64) {
        if amount_usd.is_finite() && amount_usd > 0.0 {
            self.spent_usd = (self.spent_usd - amount_usd).max(0.0);
        }
    }

    /// Record spend discovered after the fact (credit redemption amounts).
    fn record(&mut self, amount_usd: f64) {
        if amount_usd.is_finite() && amount_usd > 0.0 {
            self.spent_usd += amount_usd;
        }
    }
}

fn usd_to_cents(usd: f64) -> i64 {
    (usd * 100.0).round() as i64
}

pub(crate) fn cents_to_usd(cents: i64) -> f64 {
    cents as f64 / 100.0
}

/// Read the authenticated key's row for budget display / gating. `None`
/// when there is no key context (or the key was revoked mid-request).
pub(crate) fn read_key(
    state: &McpState,
    key_id: Option<&str>,
) -> Option<owallet_db::ProviderKeyRow> {
    let id = key_id?;
    state.db.lock().ok()?.read_provider_key(id).ok()?
}

/// The per-request spend cap in effect for this request: the wallet's
/// dashboard-set override when present (read per request, so an edit
/// applies without restarting the server), else the construction-time
/// fallback (`OWALLET_V1_SPEND_CAP_USD` env override or
/// [`DEFAULT_SPEND_CAP_USD`]).
fn effective_spend_cap(ctx: &Ctx) -> f64 {
    ctx.mcp
        .db
        .lock()
        .ok()
        .and_then(|db| db.read_spend_cap_usd_cents().ok().flatten())
        .map(|cents| cents as f64 / 100.0)
        .unwrap_or(ctx.spend_cap_usd)
}

/// `Some(refusal)` when the key's daily budget is spent. Checked at
/// request start (clean refusal before any order is placed) and before
/// each loop turn (mid-request exhaustion breaks to the landing turn
/// instead — see the loop docs).
fn exhausted_key_budget(state: &McpState, key_id: Option<&str>) -> Option<OpenAiError> {
    let key = read_key(state, key_id)?;
    if key.remaining_today_usd_cents() != Some(0) {
        return None;
    }
    Some(OpenAiError::PaymentRequired(format!(
        "daily budget exhausted: this key's ${:.2} daily budget is spent — it resets at \
         midnight in the wallet's timezone, and the wallet owner can raise it from the \
         owallet dashboard",
        key.daily_budget_usd_cents.unwrap_or(0) as f64 / 100.0
    )))
}

/// Reserve `amount_usd` against the key's persistent budget, atomically.
/// `Ok` when no key is being tracked (shouldn't happen for spend tools) or
/// the amount fit; `Err` carries the model-facing refusal.
pub(crate) fn reserve_key_budget(
    state: &McpState,
    key_id: Option<&str>,
    amount_usd: f64,
) -> ReserveResult {
    let Some(id) = key_id else {
        return Ok(());
    };
    let cents = usd_to_cents(amount_usd);
    if cents <= 0 {
        return Ok(());
    }
    let outcome = state
        .db
        .lock()
        .map_err(|e| format!("db mutex: {e}"))?
        .try_reserve_provider_key_spend(id, cents)
        .map_err(|e| format!("budget check failed: {e}"))?;
    match outcome {
        owallet_db::BudgetReservation::Reserved => Ok(()),
        owallet_db::BudgetReservation::OverBudget {
            daily_budget_usd_cents,
            remaining_today_usd_cents,
        } => Err(format!(
            "key budget exceeded: ${:.2} requested but only ${:.2} of this key's ${:.2} \
             daily budget remains — it resets at the wallet's local midnight, and the \
             wallet owner can raise it from the wallet dashboard",
            amount_usd,
            cents_to_usd(remaining_today_usd_cents),
            cents_to_usd(daily_budget_usd_cents),
        )),
        owallet_db::BudgetReservation::KeyMissing => {
            Err("this API key has been revoked".to_string())
        }
    }
}

pub(crate) type ReserveResult = std::result::Result<(), String>;

/// Hand back a key-budget reservation whose payment never moved funds.
/// Best-effort: a failure here strands allowance (safe direction).
pub(crate) fn release_key_budget(state: &McpState, key_id: Option<&str>, amount_usd: f64) {
    let Some(id) = key_id else { return };
    let cents = usd_to_cents(amount_usd);
    if cents <= 0 {
        return;
    }
    if let Ok(db) = state.db.lock() {
        let _ = db.release_provider_key_spend(id, cents);
    }
}

/// Record key-budget spend knowable only after the fact (credit
/// redemptions). Best-effort in the same direction as the in-memory
/// ledger: an overshoot just makes the key refuse everything after.
/// Stamp a projected wallet-tool result with the moment it was read, in
/// the wallet's timezone. The /v1 conversation resends its full history
/// every turn, so old tool results ride along forever — and a model with
/// a balances snapshot in context will happily present it as current
/// instead of re-calling the tool. A visible read-time makes staleness
/// legible to the model and, when it slips through anyway, to the user
/// reading the relayed answer.
fn stamp_as_of(state: &McpState, mut out: Value) -> Value {
    let tz_name = state
        .db
        .lock()
        .ok()
        .and_then(|db| db.read_timezone().ok().flatten());
    let tz = crate::timefmt::wallet_tz(tz_name.as_deref());
    if let Some(map) = out.as_object_mut() {
        map.insert("as_of".into(), json!(crate::timefmt::as_of_now(tz)));
    }
    out
}

pub(crate) fn record_key_budget(state: &McpState, key_id: Option<&str>, amount_usd: f64) {
    let Some(id) = key_id else { return };
    let cents = usd_to_cents(amount_usd);
    if cents <= 0 {
        return;
    }
    if let Ok(db) = state.db.lock() {
        let _ = db.record_provider_key_spend(id, cents);
    }
}

/// Allowlist projection of `get_account_info`: balances, credits, and the
/// spend allowance. Never copies whole sub-objects — every emitted field is
/// named here, so a field added to the MCP handler later can't leak.
fn project_balances(
    data: &Value,
    ledger: &SpendLedger,
    key: Option<&owallet_db::ProviderKeyRow>,
) -> Value {
    let mut out = crate::projection::balances_map(data);
    out.insert(
        "spend_allowance".into(),
        json!({
            "cap_usd": ledger.cap_usd,
            "spent_usd": ledger.spent_usd,
            "remaining_usd": ledger.remaining_usd(),
        }),
    );
    // The key's persistent daily budget (spend keys only; resets at
    // midnight in the wallet's timezone). `null` budget/remaining means no
    // limit was set.
    if let Some(key) = key {
        out.insert("key_budget".into(), key_budget_json(key));
    }
    Value::Object(out)
}

/// The calling key's daily-budget block, shared by [`project_balances`]
/// and the `/status` endpoint. `null` budget/remaining means no limit.
fn key_budget_json(key: &owallet_db::ProviderKeyRow) -> Value {
    json!({
        "daily_budget_usd": key.daily_budget_usd_cents.map(cents_to_usd),
        "spent_today_usd": cents_to_usd(key.spent_today_usd_cents()),
        "remaining_today_usd": key.remaining_today_usd_cents().map(cents_to_usd),
    })
}

/// Shared allowlist for one marketplace listing — see
/// [`crate::projection::listing_row`], which `/v1` and the MCP transport
/// share. `detail` adds the fields an ordering flow needs.
fn project_listing(listing: &Value, detail: bool) -> Value {
    crate::projection::listing_row(listing, detail)
}

/// Allowlist projection of `list_marketplace`: compact listing rows plus
/// the pagination cursor.
fn project_marketplace(data: &Value) -> Value {
    let rows: Vec<Value> = data
        .get("data")
        .and_then(Value::as_array)
        .map(|listings| listings.iter().map(|l| project_listing(l, false)).collect())
        .unwrap_or_default();
    let mut out = serde_json::Map::new();
    out.insert("listings".into(), json!(rows));
    if let Some(cursor) = data.get("next_cursor").filter(|c| !c.is_null()) {
        out.insert("next_cursor".into(), cursor.clone());
    }
    Value::Object(out)
}

/// Allowlist projection of `get_listing` (the `{data: {...}}` envelope or
/// flat): the browse row plus the ordering-flow fields.
fn project_listing_detail(data: &Value) -> Value {
    project_listing(data.get("data").unwrap_or(data), true)
}

/// Allowlist projection of `get_wallet_orders`: one compact row per order,
/// plus the pagination cursor. Each raw row carries `settlement_tx_hash`,
/// `order_url`, tracking fields, and the `buyer_note` (an arbitrary
/// seller-bound payload that should not be re-shipped to the OpenRouter
/// seller) — none of that exists here.
fn project_orders_list(data: &Value) -> Value {
    let rows = data
        .get("data")
        .and_then(Value::as_array)
        .map(|orders| {
            orders
                .iter()
                .map(crate::projection::order_summary_row)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut out = serde_json::Map::new();
    out.insert("orders".into(), json!(rows));
    if let Some(cursor) = data.get("next_cursor").filter(|c| !c.is_null()) {
        out.insert("next_cursor".into(), cursor.clone());
    }
    Value::Object(out)
}

use crate::projection::DELIVERED_CONTENT_MODEL_CAP;

/// Allowlist projection of `get_order_status`: identity + statuses +
/// price + (once delivered) the deliverable itself, capped at
/// [`DELIVERED_CONTENT_MODEL_CAP`] on a character boundary. The raw order
/// payload carries `settlement_tx_hash` (and, on some shapes, payment
/// details) — none of that exists here.
fn project_order_status(data: &Value) -> Value {
    let order = data.get("data").unwrap_or(data);
    let mut out = serde_json::Map::new();
    if let Some(id) = order.get("id").or_else(|| order.get("order_id")) {
        out.insert("order_id".into(), id.clone());
    }
    for key in [
        "product_title",
        "payment_status",
        "fulfillment_status",
        "total_usd",
        // What the buyer actually paid after settlement refunds — a metered
        // order's total_usd stays at the deposit, so show this alongside it.
        "settled_amount_cents",
    ] {
        if let Some(v) = order.get(key) {
            out.insert(key.into(), v.clone());
        }
    }
    if let Some(url) = order
        .get("delivered_content_url")
        .and_then(Value::as_str)
        .filter(|u| !u.is_empty())
    {
        // File deliverable: the blob lives behind a marketplace download
        // URL rather than inline (not on-chain data — it's the same link
        // the web order page offers the buyer).
        out.insert("delivered_content_url".into(), json!(url));
    } else if let Some(content) = order.get("delivered_content").and_then(Value::as_str) {
        if content.len() > DELIVERED_CONTENT_MODEL_CAP {
            let mut end = DELIVERED_CONTENT_MODEL_CAP;
            while !content.is_char_boundary(end) {
                end -= 1;
            }
            out.insert("delivered_content".into(), json!(&content[..end]));
            out.insert("delivered_content_truncated".into(), json!(true));
        } else {
            out.insert("delivered_content".into(), json!(content));
        }
    }
    if let Some(v) = order.get("delivered_content_type") {
        out.insert("delivered_content_type".into(), v.clone());
    }
    Value::Object(out)
}

/// Allowlist projection of `pay_order`'s (already flat, credits-only)
/// result, plus the updated spend allowance (and key budget, if bounded).
fn project_pay_order(
    data: &Value,
    ledger: &SpendLedger,
    key: Option<&owallet_db::ProviderKeyRow>,
) -> Value {
    let mut out = crate::projection::pay_order_map(data);
    out.insert("remaining_spend_usd".into(), json!(ledger.remaining_usd()));
    insert_key_remaining(&mut out, key);
    Value::Object(out)
}

/// Allowlist projection of `buy`: the MCP result carries `tx_hash`/`txid`,
/// the payment address, and a web `order_url` — the model gets the order
/// id, the status, and the amount it asked for.
fn project_buy(
    data: &Value,
    amount_usd: f64,
    ledger: &SpendLedger,
    key: Option<&owallet_db::ProviderKeyRow>,
) -> Value {
    let mut out = serde_json::Map::new();
    if let Some(err) = data.get("error") {
        out.insert("error".into(), err.clone());
    } else {
        out.insert(
            "status".into(),
            data.get("status").cloned().unwrap_or(json!("payment_sent")),
        );
        out.insert("amount_usd".into(), json!(amount_usd));
        if let Some(v) = data.get("note") {
            out.insert("note".into(), v.clone());
        }
    }
    if let Some(id) = data.get("order_id") {
        out.insert("order_id".into(), id.clone());
    }
    out.insert("remaining_spend_usd".into(), json!(ledger.remaining_usd()));
    insert_key_remaining(&mut out, key);
    Value::Object(out)
}

/// Add the key's remaining daily budget to a spend-tool result — only
/// when the key actually has a budget, so unlimited keys stay noise-free.
fn insert_key_remaining(
    out: &mut serde_json::Map<String, Value>,
    key: Option<&owallet_db::ProviderKeyRow>,
) {
    if let Some(remaining) = key.and_then(|k| k.remaining_today_usd_cents()) {
        out.insert(
            "key_budget_remaining_today_usd".into(),
            json!(cents_to_usd(remaining)),
        );
    }
}

/// Execute one wallet tool call and return its (projected) result as the
/// `content` string for a `role: "tool"` message. Like
/// [`execute_tool_call`], never `Err` — failures become `{"error": ...}`
/// strings the model can react to.
async fn execute_wallet_tool(
    state: &McpState,
    name: &str,
    arguments: &Value,
    can_spend: bool,
    key_id: Option<&str>,
    ledger: &mut SpendLedger,
) -> String {
    let Some(spec) = WALLET_TOOLS.iter().find(|t| t.name == name) else {
        return json!({"error": format!("unknown tool '{name}'")}).to_string();
    };
    if spec.spend && !can_spend {
        return json!({
            "error": "this API key is chat-only — wallet spending tools require a key \
                      minted with the spend scope (dashboard: create a provider key \
                      with spending allowed)"
        })
        .to_string();
    }

    match name {
        GET_BALANCES_TOOL => {
            match crate::tools::dispatch(state, "get_account_info", json!({}), None).await {
                Ok(out) => {
                    let key = read_key(state, key_id);
                    stamp_as_of(state, project_balances(&out.data, ledger, key.as_ref()))
                        .to_string()
                }
                Err(e) => json!({"error": e.to_string()}).to_string(),
            }
        }
        BROWSE_MARKETPLACE_TOOL => {
            match crate::tools::dispatch(state, "list_marketplace", arguments.clone(), None).await {
                Ok(out) => project_marketplace(&out.data).to_string(),
                Err(e) => json!({"error": e.to_string()}).to_string(),
            }
        }
        GET_LISTING_TOOL => {
            match crate::tools::dispatch(state, "get_listing", arguments.clone(), None).await {
                Ok(out) => project_listing_detail(&out.data).to_string(),
                Err(e) => json!({"error": e.to_string()}).to_string(),
            }
        }
        LIST_ORDERS_TOOL => {
            match crate::tools::dispatch(state, "get_wallet_orders", arguments.clone(), None).await
            {
                Ok(out) => stamp_as_of(state, project_orders_list(&out.data)).to_string(),
                Err(e) => json!({"error": e.to_string()}).to_string(),
            }
        }
        GET_ORDER_STATUS_TOOL => {
            // Ask the MCP handler to keep delivered_content inline (it
            // otherwise strips large blobs to a local-cache pointer the
            // model can't follow); the projection applies its own cap.
            let mut dispatch_args = arguments.clone();
            if let Some(obj) = dispatch_args.as_object_mut() {
                obj.insert("include_delivered_content".into(), json!(true));
            }
            match crate::tools::dispatch(state, "get_order_status", dispatch_args, None).await {
                Ok(out) => stamp_as_of(state, project_order_status(&out.data)).to_string(),
                Err(e) => json!({"error": e.to_string()}).to_string(),
            }
        }
        CREATE_ORDER_TOOL => {
            match crate::tools::dispatch(state, "create_order", arguments.clone(), None).await {
                // Creation itself is unpaid, so nothing is recorded against
                // the ledger — pay_order settles (and accounts for) it.
                Ok(out) => project_order_status(&out.data).to_string(),
                Err(e) => json!({"error": e.to_string()}).to_string(),
            }
        }
        PAY_ORDER_TOOL => {
            if ledger.remaining_usd() <= 0.0 {
                return json!({
                    "error": format!(
                        "spend cap exhausted: this request's ${:.2} allowance is spent",
                        ledger.cap_usd
                    )
                })
                .to_string();
            }
            // Same soft gate against the key's persistent budget: a
            // redemption's amount is knowable only after the fact, so an
            // exhausted budget refuses up front and the actual amount is
            // recorded after.
            if let Some(key) = read_key(state, key_id) {
                if key.remaining_today_usd_cents() == Some(0) {
                    return json!({
                        "error": format!(
                            "key budget exhausted: this key's ${:.2} daily budget is \
                             spent — it resets at the wallet's local midnight, and the \
                             wallet owner can raise it from the wallet dashboard",
                            cents_to_usd(key.daily_budget_usd_cents.unwrap_or(0))
                        )
                    })
                    .to_string();
                }
            }
            match crate::tools::dispatch(state, "pay_order", arguments.clone(), None).await {
                Ok(out) => {
                    // What the redemption actually applied counts against
                    // the allowance — knowable only after the fact.
                    if let Some(cents) = out
                        .data
                        .get("amount_redeemed_cents")
                        .and_then(Value::as_f64)
                    {
                        ledger.record(cents / 100.0);
                        record_key_budget(state, key_id, cents / 100.0);
                    }
                    let key = read_key(state, key_id);
                    project_pay_order(&out.data, ledger, key.as_ref()).to_string()
                }
                Err(e) => json!({"error": e.to_string()}).to_string(),
            }
        }
        BUY_CREDITS_TOOL => {
            let amount_usd = arguments
                .get("amount_usd")
                .and_then(Value::as_f64)
                .unwrap_or(f64::NAN);
            if let Err(reason) = ledger.try_spend(amount_usd) {
                return json!({"error": reason}).to_string();
            }
            // The amount is in the arguments, so the key budget reserves up
            // front too — atomically, so parallel requests on the same key
            // can't both squeeze through the last dollar.
            if let Err(reason) = reserve_key_budget(state, key_id, amount_usd) {
                ledger.release(amount_usd);
                return json!({"error": reason}).to_string();
            }
            match crate::tools::dispatch(state, "buy", arguments.clone(), None).await {
                Ok(out) => {
                    // `buy`'s soft-error shapes mean the payment was NOT
                    // sent (order created but unpaid) — nothing left the
                    // wallet, so the reservation goes back.
                    if out.data.get("error").is_some() {
                        ledger.release(amount_usd);
                        release_key_budget(state, key_id, amount_usd);
                    }
                    let key = read_key(state, key_id);
                    project_buy(&out.data, amount_usd, ledger, key.as_ref()).to_string()
                }
                Err(e) => {
                    // Hard errors fire before any payment is broadcast
                    // (argument/order-creation failures) — release too.
                    ledger.release(amount_usd);
                    release_key_budget(state, key_id, amount_usd);
                    json!({"error": e.to_string()}).to_string()
                }
            }
        }
        _ => json!({"error": format!("unknown tool '{name}'")}).to_string(),
    }
}

// ---------------------------------------------------------------------------
// POST /v1/chat/completions
// ---------------------------------------------------------------------------

/// `messages` is deserialized as raw JSON rather than a strict typed
/// struct: a tool-calling conversation needs shapes plain `{role,
/// content}` can't carry (an assistant's own tool-call turn, a tool's
/// result keyed by `tool_call_id`), and — since this endpoint's own
/// agentic loop appends its own assistant/tool turns to the *same*
/// messages list before looping — one untyped representation for "the
/// caller's messages" and "what the loop appends" is simpler than two.
/// `normalize_message` is where the actual field-level handling lives.
#[derive(Debug, Deserialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<Value>,
    #[serde(default)]
    stream: bool,
    /// Caller-supplied tool definitions switch the request into
    /// **passthrough mode**: the server-side roster (`run_python`, wallet
    /// tools, listing tools) is not advertised, the caller's definitions
    /// are forwarded to the listing verbatim, and any tool_calls the model
    /// emits come back to the caller unexecuted (`finish_reason:
    /// "tool_calls"`) — the caller owns the conversation history, so it
    /// must own tool execution too, or rounds it never witnessed would
    /// vanish from every later request. Absent (or empty), the endpoint
    /// keeps its original transparent server-side loop.
    #[serde(default)]
    tools: Option<Vec<Value>>,
    /// Forwarded with the caller's tools; ignored without them (the
    /// server-side loop sets its own).
    #[serde(default)]
    tool_choice: Option<Value>,
}

impl ChatCompletionRequest {
    /// Passthrough mode is opted into by sending a non-empty `tools` array
    /// — the shape every OpenAI-style agent client (opencode/norm included)
    /// produces when it has an executor of its own. A bare `tools: []`
    /// stays in server mode: some SDKs emit the empty array for plain chat.
    fn client_tools(&self) -> Option<&[Value]> {
        self.tools.as_deref().filter(|t| !t.is_empty())
    }
}

/// `content` is a bare string in the common case, but some OpenAI-compatible
/// clients send the multipart form (`[{type:"text", text:"..."}]`) even for
/// plain chat. Text parts are concatenated; non-text parts (image/audio) are
/// silently dropped — everything downstream of this endpoint is text-only.
fn message_text(content: &Value) -> Option<String> {
    match content {
        Value::String(s) => Some(s.clone()),
        Value::Array(parts) => {
            let text: String = parts
                .iter()
                .filter(|p| p.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect();
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    }
}

/// Keeps `role` plus whichever of `content` / `tool_calls` / `tool_call_id`
/// are present and well-formed — mirroring the marketplace listing's own
/// `coerce_message` so a message survives the round trip through both
/// sides unchanged. `None` for an entry with no role, or with none of the
/// three payload shapes (nothing worth sending).
fn normalize_message(entry: &Value) -> Option<Value> {
    let role = entry.get("role")?.as_str()?;

    let mut out = serde_json::Map::new();
    out.insert("role".to_string(), json!(role));

    if let Some(text) = entry.get("content").and_then(message_text) {
        out.insert("content".to_string(), json!(text));
    }
    if let Some(tool_calls) = entry
        .get("tool_calls")
        .filter(|v| v.as_array().is_some_and(|a| !a.is_empty()))
    {
        out.insert("tool_calls".to_string(), tool_calls.clone());
    }
    if let Some(tool_call_id) = entry.get("tool_call_id").and_then(Value::as_str) {
        out.insert("tool_call_id".to_string(), json!(tool_call_id));
    }

    if !out.contains_key("content")
        && !out.contains_key("tool_calls")
        && !out.contains_key("tool_call_id")
    {
        return None;
    }
    Some(Value::Object(out))
}

fn normalize_messages(raw: &[Value]) -> Result<Vec<Value>, OpenAiError> {
    let messages: Vec<Value> = raw.iter().filter_map(normalize_message).collect();
    if messages.is_empty() {
        return Err(OpenAiError::InvalidRequest(
            "messages must contain at least one usable entry".into(),
        ));
    }
    Ok(messages)
}

async fn chat_completions(
    State(ctx): State<Ctx>,
    headers: HeaderMap,
    Json(req): Json<ChatCompletionRequest>,
) -> Response {
    let (mcp, can_spend, key_id) = match authenticate_provider_key(&ctx.mcp, &headers) {
        Ok(auth) => auth,
        Err(e) => return e.into_response(),
    };
    let ctx = Ctx {
        mcp,
        can_spend,
        key_id,
        ..ctx
    };
    // The daily budget bounds *everything* the key costs — each chat turn
    // is itself a paid order — so an exhausted key refuses cleanly before
    // any order is placed rather than erroring mid-conversation.
    if let Some(exhausted) = exhausted_key_budget(&ctx.mcp, ctx.key_id.as_deref()) {
        return exhausted.into_response();
    }
    if let Err(e) = validate_request(&ctx.mcp, &req).await {
        return e.into_response();
    }

    if req.stream {
        stream_chat_completion(ctx, req)
    } else {
        match buffered_chat_completion(&ctx, req).await {
            Ok(resp) => Json(resp).into_response(),
            Err(e) => e.into_response(),
        }
    }
}

/// Check an OpenAI-compatible bearer credential and return state pinned to
/// its wallet, plus whether the key's scopes allow the wallet spending
/// tools and (when they do) the key id the spending budget is accounted
/// against. Provider key verifiers live in SQLite, so a database copy does
/// not reveal usable spending credentials.
fn authenticate_provider_key(
    state: &McpState,
    headers: &HeaderMap,
) -> Result<(McpState, bool, Option<String>), OpenAiError> {
    let value = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|token| !token.is_empty())
        .ok_or_else(|| OpenAiError::Unauthorized("missing provider API key".into()))?;
    let key = state
        .db
        .lock()
        .map_err(|e| OpenAiError::internal(format!("db mutex: {e}")))?
        .read_provider_key_auth(value)
        .map_err(|e| OpenAiError::internal(format!("provider key lookup: {e}")))?
        .ok_or_else(|| OpenAiError::Unauthorized("invalid provider API key".into()))?;
    let can_spend = key.can_spend();
    Ok((state.with_npub(Some(key.npub)), can_spend, Some(key.id)))
}

async fn validate_request(
    state: &McpState,
    req: &ChatCompletionRequest,
) -> Result<(), OpenAiError> {
    if req.messages.is_empty() {
        return Err(OpenAiError::InvalidRequest(
            "messages must contain at least one entry".into(),
        ));
    }
    // Accepted unconditionally, without a live catalog fetch -- see
    // `DEFAULT_MODEL`'s doc comment for why this needs to work even when
    // Overpay/the listing can't be reached to validate a real model id.
    if req.model == DEFAULT_MODEL {
        return Ok(());
    }
    let models = resolve_models(state).await?;
    if !models.iter().any(|m| m == &req.model) {
        return Err(OpenAiError::InvalidRequest(format!(
            "The model `{}` does not exist or is not available. \
             See GET /v1/models for the supported list.",
            req.model
        )));
    }
    Ok(())
}

// ---- shared: place + pay for any order, poll it to a terminal status ----

/// Create and pay for an order against any listing this endpoint knows
/// about, via merchant credits under `seller_slug`. Shared by the
/// OpenRouter turn and each `run_python` tool execution — the only thing
/// that differs between them is which listing/seller/buyer_note is used.
async fn place_and_pay_order(
    state: &McpState,
    auth: &OwnedAuth,
    listing_id: &str,
    seller_slug: &str,
    buyer_note: &Value,
    key_id: Option<&str>,
) -> Result<(String, i64), OpenAiError> {
    // Strings pass through verbatim, matching the MCP `create_order`
    // convention — a `buyer_input :text` listing's bot reads the note as
    // plain text, and JSON-encoding would hand it literal quotes.
    let note_str = match buyer_note {
        Value::String(s) => s.clone(),
        other => serde_json::to_string(other)
            .map_err(|e| OpenAiError::internal(format!("could not encode buyer_note: {e}")))?,
    };

    // One request creates AND settles when the marketplace understands
    // `pay: "merchant_credits"` (its response then carries a `payment`
    // key) — halving the Rails round trips of the hottest call in the
    // module. An older marketplace ignores the param and returns only
    // the order; the separate redeem call below covers it.
    let order = state
        .overpay
        .create_and_pay_order_value(listing_id, Some(&note_str), auth.as_auth())
        .await?;
    let order_id = order
        .get("data")
        .and_then(|d| d.get("id"))
        .and_then(Value::as_str)
        .ok_or_else(|| OpenAiError::internal("create_order response missing id"))?
        .to_string();

    let payment = match order.get("payment") {
        Some(p) => p.clone(),
        None => {
            let redeem = state
                .overpay
                .redeem_merchant_credits_value(seller_slug, &order_id, auth.as_auth())
                .await?;
            redeem.get("data").cloned().unwrap_or(Value::Null)
        }
    };

    let status = payment.get("status").and_then(Value::as_str).unwrap_or("");
    if status != "fully_paid" && status != "already_paid" {
        let message = payment
            .get("message")
            .or_else(|| payment.get("error"))
            .and_then(Value::as_str)
            .unwrap_or("insufficient Overpay merchant credits");
        return Err(OpenAiError::PaymentRequired(format!(
            "{message} — load more with the wallet's `load_core_credits` MCP tool or the dashboard"
        )));
    }

    // The endpoint's own operating spend counts against the key's daily
    // budget too — a budget is a bound on what the key costs per day, not
    // just on what the wallet tools move. (The per-request SpendLedger
    // deliberately still excludes it: iterations bound per-request
    // operating spend.) Recorded after the fact like a redemption — at
    // the gross deposit, which is all that's knowable at pay time; a
    // metered listing settles below it after delivery, so callers hand
    // the returned cents to [`net_key_budget_from_delivery`] once they
    // hold the terminal snapshot.
    let mut redeemed_cents: i64 = 0;
    if let Some(cents) = payment.get("amount_redeemed_cents").and_then(Value::as_f64) {
        record_key_budget(state, key_id, cents / 100.0);
        redeemed_cents = cents.round() as i64;
    }

    Ok((order_id, redeemed_cents))
}

/// Net a metered order's settlement refund back out of the key's daily
/// budget. The pay step records the gross deposit; a metered listing
/// (OpenRouter inference) settles the order down to its actual cost
/// right after delivery and states that final `charged_cents` in the
/// delivered payload, so the difference goes back to the budget — the
/// budget stays a bound on what the key actually cost, not on gross
/// deposits. A delivery without `charged_cents` (run_python, listing
/// tools, sellers that don't meter) nets nothing, which errs on the
/// conservative side; the upstream-error payload carries
/// `charged_cents: 0` alongside its credit refund, so a failed turn
/// hands its whole deposit back here too.
fn net_key_budget_from_delivery(
    state: &McpState,
    key_id: Option<&str>,
    snap: &Value,
    redeemed_cents: i64,
) {
    let Some(id) = key_id else { return };
    if redeemed_cents <= 0 {
        return;
    }
    let Ok(inner) = delivered_content_json(snap) else {
        return;
    };
    let Some(charged) = inner.get("charged_cents").and_then(Value::as_i64) else {
        return;
    };
    let refund = (redeemed_cents - charged.max(0)).clamp(0, redeemed_cents);
    if refund <= 0 {
        return;
    }
    if let Ok(db) = state.db.lock() {
        let _ = db.release_provider_key_spend(id, refund);
    }
}

/// Poll an order silently until it reaches a terminal status. Used by the
/// buffered path and by every `run_python` tool execution (which never
/// streams — the caller only sees the outer OpenRouter turns' text). The
/// streaming path's own per-turn loop duplicates the polling shape rather
/// than calling this, since it also has to diff `partial_content` and
/// yield SSE events along the way.
async fn wait_for_order_terminal(
    state: &McpState,
    auth: &OwnedAuth,
    order_id: &str,
    timeout: Duration,
    poll: Duration,
) -> Result<Value, OpenAiError> {
    let start = Instant::now();
    // This caller never streams the partial buffer, so passing the last
    // seen seq back as `since_seq` lets the marketplace omit it from
    // every poll — the buffer can be 32 KB, re-downloaded each second.
    let mut last_seq = 0u64;
    loop {
        let snap = state
            .overpay
            .get_order_value_since(order_id, Some(last_seq), auth.as_auth())
            .await?;
        last_seq = last_seq.max(partial_output(&snap).1.unwrap_or(0));
        if is_terminal(order_status(&snap)) {
            return Ok(snap);
        }
        if start.elapsed() >= timeout {
            return Err(OpenAiError::UpstreamFailure(format!(
                "order {order_id} did not complete within {}s",
                timeout.as_secs()
            )));
        }
        tokio::time::sleep(poll).await;
    }
}

/// What one order actually cost the wallet: the seller's metered
/// `charged_cents` when the delivery states one, else the gross deposit.
/// The mirror image of the refund [`net_key_budget_from_delivery`] hands
/// back to the key budget, so the two always agree on what a turn cost.
fn net_charged_cents(snap: &Value, redeemed_cents: i64) -> i64 {
    if redeemed_cents <= 0 {
        return 0;
    }
    delivered_content_json(snap)
        .ok()
        .and_then(|inner| inner.get("charged_cents").and_then(Value::as_i64))
        .map(|charged| charged.clamp(0, redeemed_cents))
        .unwrap_or(redeemed_cents)
}

/// Everything one chat completion spent and consumed, accumulated across
/// every order it placed — the OpenRouter turns *and* the tool calls, each
/// of which is a separately paid marketplace order. A tool call can cost
/// far more than the inference around it (image generation), so a total
/// that counted only the model turns would understate real spend badly.
///
/// Reported back on the response so a client can show what the turn
/// actually cost instead of estimating tokens × a list price it has no
/// way to know (norm's sidebar does exactly this).
#[derive(Default, Clone, Copy, Debug)]
pub(crate) struct TurnUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
    charged_cents: i64,
}

impl TurnUsage {
    /// Charge one settled order against the turn. Pair every
    /// `net_key_budget_from_delivery` with this: same snapshot, same
    /// deposit, so the budget and the reported cost cannot drift.
    fn add_order(&mut self, snap: &Value, redeemed_cents: i64) {
        self.charged_cents = self
            .charged_cents
            .saturating_add(net_charged_cents(snap, redeemed_cents));
    }

    /// Token counts from an OpenRouter delivery. Tool-call orders have no
    /// tokens of their own — only their cost lands on the turn.
    fn add_tokens(&mut self, delivered: &OpenRouterDelivered) {
        self.prompt_tokens = self.prompt_tokens.saturating_add(delivered.prompt_tokens);
        self.completion_tokens = self
            .completion_tokens
            .saturating_add(delivered.completion_tokens);
    }

    /// OpenAI's `usage` shape plus two extensions: `cost` (USD, the
    /// convention OpenRouter set) and `charged_cents`, the authoritative
    /// integer — real money should not round-trip through a float.
    fn to_json(self) -> Value {
        json!({
            "prompt_tokens": self.prompt_tokens,
            "completion_tokens": self.completion_tokens,
            "total_tokens": self.prompt_tokens.saturating_add(self.completion_tokens),
            "cost": self.charged_cents as f64 / 100.0,
            "charged_cents": self.charged_cents,
        })
    }
}

/// What the OpenRouter listing actually delivered, parsed out of the order
/// snapshot's (JSON-string-encoded) `delivered_content`.
struct OpenRouterDelivered {
    text: String,
    model: String,
    error: bool,
    tool_calls: Vec<Value>,
    prompt_tokens: u64,
    completion_tokens: u64,
}

fn extract_openrouter_delivered(snap: &Value) -> Result<OpenRouterDelivered, OpenAiError> {
    let inner = delivered_content_json(snap)?;
    Ok(OpenRouterDelivered {
        text: inner
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        model: inner
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        error: inner.get("error").and_then(Value::as_bool).unwrap_or(false),
        tool_calls: inner
            .get("tool_calls")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        // The seller states the upstream model's own counts; a seller that
        // doesn't meter simply reports none and the turn contributes zero
        // tokens rather than a guess.
        prompt_tokens: delivered_usage_tokens(&inner, "prompt_tokens"),
        completion_tokens: delivered_usage_tokens(&inner, "completion_tokens"),
    })
}

fn delivered_usage_tokens(inner: &Value, field: &str) -> u64 {
    inner
        .pointer("/usage")
        .and_then(|usage| usage.get(field))
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

/// The Python listing's delivered `{stdout, stderr, exit_code, duration_ms,
/// timed_out}` — returned as-is (not restructured into a Rust type) since
/// it becomes the `content` of a tool-result message fed straight back to
/// the model, which reads the same shape the listing already documents.
fn extract_python_delivered(snap: &Value) -> Result<Value, OpenAiError> {
    delivered_content_json(snap)
}

fn delivered_content_json(snap: &Value) -> Result<Value, OpenAiError> {
    let data = snap.get("data").unwrap_or(snap);
    let raw = data
        .get("delivered_content")
        .and_then(Value::as_str)
        .ok_or_else(|| OpenAiError::internal("order has no delivered_content"))?;
    serde_json::from_str(raw)
        .map_err(|e| OpenAiError::internal(format!("delivered_content is not valid JSON: {e}")))
}

fn order_status(snap: &Value) -> Option<&str> {
    snap.get("data")
        .and_then(|d| d.get("fulfillment_status"))
        .or_else(|| snap.get("fulfillment_status"))
        .and_then(Value::as_str)
}

fn is_terminal(status: Option<&str>) -> bool {
    status == Some("delivered")
        || status
            .map(|s| WAIT_TERMINAL_STATUSES.contains(&s))
            .unwrap_or(false)
}

// ---- tool execution: run_python, backed by a real second paid order ----

/// Runs whatever tool the model asked for and returns its result as the
/// `content` string for a `role: "tool"` message. Never returns
/// `Result::Err` to the caller — a failed tool execution becomes an
/// `{"error": ...}` JSON string the model itself sees on the next turn and
/// can react to (retry differently, apologize, ask a clarifying question),
/// same as how a real coding agent would surface a failed tool call rather
/// than crashing the whole conversation over it.
#[allow(clippy::too_many_arguments)]
async fn execute_tool_call(
    ctx: &Ctx,
    auth: &OwnedAuth,
    call: &Value,
    ledger: &mut SpendLedger,
    usage: &mut TurnUsage,
) -> String {
    let state = &ctx.mcp;
    let (timeout, poll) = (ctx.timeout, ctx.poll);
    let (can_spend, key_id) = (ctx.can_spend, ctx.key_id.as_deref());
    let name = call
        .pointer("/function/name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let listing_tool = if name != RUN_PYTHON_TOOL_NAME && !is_wallet_tool(name) {
        listing_tool_named(ctx, name).await
    } else {
        None
    };
    if name != RUN_PYTHON_TOOL_NAME && !is_wallet_tool(name) && listing_tool.is_none() {
        return json!({"error": format!("unknown tool '{name}'")}).to_string();
    }

    let arguments_str = call
        .pointer("/function/arguments")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let arguments: Value = match serde_json::from_str(arguments_str) {
        Ok(v) => v,
        Err(e) => {
            return json!({"error": format!("tool call arguments are not valid JSON: {e}")})
                .to_string()
        }
    };

    if is_wallet_tool(name) {
        return execute_wallet_tool(state, name, &arguments, can_spend, key_id, ledger).await;
    }

    if let Some(tool) = listing_tool {
        return match run_listing_tool(state, auth, &tool, &arguments, timeout, poll, key_id, usage)
            .await
        {
            Ok(result) => result.to_string(),
            Err(e) => json!({"error": e.message()}).to_string(),
        };
    }

    match run_python_tool(state, auth, &arguments, timeout, poll, key_id, usage).await {
        Ok(result) => result.to_string(),
        Err(e) => json!({"error": e.message()}).to_string(),
    }
}

pub(crate) async fn run_python_tool(
    state: &McpState,
    auth: &OwnedAuth,
    arguments: &Value,
    timeout: Duration,
    poll: Duration,
    key_id: Option<&str>,
    usage: &mut TurnUsage,
) -> Result<Value, OpenAiError> {
    let listing_id = resolve_python_listing_id(state).await?;
    let (order_id, redeemed_cents) = place_and_pay_order(
        state,
        auth,
        &listing_id,
        PYTHON_SELLER_SLUG,
        arguments,
        key_id,
    )
    .await?;
    let snap = wait_for_order_terminal(state, auth, &order_id, timeout, poll).await?;
    net_key_budget_from_delivery(state, key_id, &snap, redeemed_cents);
    usage.add_order(&snap, redeemed_cents);
    extract_python_delivered(&snap)
}

// ---- buffered ----

#[derive(Serialize)]
struct ChatCompletionResponse {
    id: String,
    object: &'static str,
    created: i64,
    model: String,
    choices: Vec<ChatCompletionChoice>,
    /// What the turn actually consumed and cost. See [`TurnUsage`].
    usage: Value,
}

#[derive(Serialize)]
struct ChatCompletionChoice {
    index: u32,
    message: ChatMessageOut,
    finish_reason: &'static str,
}

#[derive(Serialize)]
struct ChatMessageOut {
    role: &'static str,
    /// `null` (not `""`) on a pure tool-call turn — OpenAI clients switch
    /// on that distinction when deciding whether the turn carried prose.
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<Value>>,
}

/// One OpenRouter turn's outcome once the agentic loop is done: either a
/// final answer, or (internally, before the loop decides) a tool call to
/// execute and feed back. `tool_calls` is non-empty only in passthrough
/// mode, where the caller executes them.
struct AgentResult {
    text: String,
    model: String,
    order_id: String,
    tool_calls: Vec<Value>,
    usage: TurnUsage,
}

/// Runs the OpenRouter <-> `run_python` loop to a final answer: place +
/// pay for an OpenRouter turn, and if it comes back with tool_calls,
/// execute each (a real, separately-paid order against the Python
/// listing), record the assistant's tool-call turn and each tool's result
/// on `messages`, and loop. Ends when a turn produces no tool_calls, or
/// the [`MAX_TOOL_ITERATIONS`] safety cap is hit.
async fn run_agentic_loop(
    ctx: &Ctx,
    auth: &OwnedAuth,
    mut messages: Vec<Value>,
    requested_model: &str,
) -> Result<AgentResult, OpenAiError> {
    let listing_id = resolve_openrouter_listing_id(&ctx.mcp).await?;
    let defs = tool_defs(ctx, ctx.can_spend).await?;
    let mut ledger = SpendLedger::new(effective_spend_cap(ctx));
    let mut last_model = requested_model.to_string();
    let mut usage = TurnUsage::default();

    for _ in 0..MAX_TOOL_ITERATIONS {
        // Mid-request exhaustion of the daily budget breaks to the landing
        // turn (which costs one more turn — accepted overshoot) so the
        // model reports what it already did instead of a dropped request.
        if exhausted_key_budget(&ctx.mcp, ctx.key_id.as_deref()).is_some() {
            break;
        }
        let buyer_note = json!({
            "model": requested_model,
            "messages": messages,
            "tools": defs,
            "tool_choice": "auto",
        });
        let (order_id, redeemed_cents) = place_and_pay_order(
            &ctx.mcp,
            auth,
            &listing_id,
            OPENROUTER_SELLER_SLUG,
            &buyer_note,
            ctx.key_id.as_deref(),
        )
        .await?;
        let snap =
            wait_for_order_terminal(&ctx.mcp, auth, &order_id, ctx.timeout, ctx.poll).await?;
        net_key_budget_from_delivery(&ctx.mcp, ctx.key_id.as_deref(), &snap, redeemed_cents);
        usage.add_order(&snap, redeemed_cents);
        let delivered = extract_openrouter_delivered(&snap)?;
        if delivered.error {
            return Err(OpenAiError::UpstreamFailure(delivered.text));
        }
        usage.add_tokens(&delivered);
        if !delivered.model.is_empty() {
            last_model = delivered.model;
        }

        if delivered.tool_calls.is_empty() {
            return Ok(AgentResult {
                text: delivered.text,
                model: last_model,
                order_id,
                tool_calls: Vec::new(),
                usage,
            });
        }

        messages.push(json!({
            "role": "assistant",
            "content": if delivered.text.is_empty() { Value::Null } else { json!(delivered.text) },
            "tool_calls": delivered.tool_calls,
        }));
        for call in &delivered.tool_calls {
            let result_text = execute_tool_call(ctx, auth, call, &mut ledger, &mut usage).await;
            let tool_call_id = call.get("id").and_then(Value::as_str).unwrap_or_default();
            messages.push(
                json!({ "role": "tool", "tool_call_id": tool_call_id, "content": result_text }),
            );
        }
    }

    // Cap reached. Real orders may have been created and paid along the
    // way — an error now would throw that context away. One final turn
    // with tools disabled forces the model to report what it actually did;
    // only if it *still* yields no text does the request fail.
    let buyer_note = json!({
        "model": requested_model,
        "messages": messages,
        "tools": defs,
        "tool_choice": "none",
    });
    let (order_id, redeemed_cents) = place_and_pay_order(
        &ctx.mcp,
        auth,
        &listing_id,
        OPENROUTER_SELLER_SLUG,
        &buyer_note,
        ctx.key_id.as_deref(),
    )
    .await?;
    let snap = wait_for_order_terminal(&ctx.mcp, auth, &order_id, ctx.timeout, ctx.poll).await?;
    net_key_budget_from_delivery(&ctx.mcp, ctx.key_id.as_deref(), &snap, redeemed_cents);
    usage.add_order(&snap, redeemed_cents);
    let delivered = extract_openrouter_delivered(&snap)?;
    if delivered.error {
        return Err(OpenAiError::UpstreamFailure(delivered.text));
    }
    usage.add_tokens(&delivered);
    if !delivered.model.is_empty() {
        last_model = delivered.model;
    }
    if delivered.text.is_empty() {
        return Err(OpenAiError::UpstreamFailure(format!(
            "the model kept calling tools past the {MAX_TOOL_ITERATIONS}-iteration safety cap \
             and gave no final answer even with tools disabled — stopping rather than \
             spending further"
        )));
    }
    Ok(AgentResult {
        text: delivered.text,
        model: last_model,
        order_id,
        tool_calls: Vec::new(),
        usage,
    })
}

/// One passthrough-mode turn: the caller's tool definitions forwarded to
/// the listing verbatim, and whatever the model does — prose, tool_calls,
/// or both — handed straight back. No server roster, no execution, no
/// iteration cap: the caller runs the loop, so each request is exactly one
/// paid turn (still recorded against the key's daily budget like any
/// other).
async fn run_passthrough_turn(
    ctx: &Ctx,
    auth: &OwnedAuth,
    messages: Vec<Value>,
    requested_model: &str,
    tools: &[Value],
    tool_choice: Option<&Value>,
) -> Result<AgentResult, OpenAiError> {
    let listing_id = resolve_openrouter_listing_id(&ctx.mcp).await?;
    let mut buyer_note = json!({
        "model": requested_model,
        "messages": messages,
        "tools": tools,
    });
    // Only forwarded when the caller set one — the listing (and OpenRouter
    // beneath it) default to "auto" on their own.
    if let Some(choice) = tool_choice {
        buyer_note["tool_choice"] = choice.clone();
    }
    let (order_id, redeemed_cents) = place_and_pay_order(
        &ctx.mcp,
        auth,
        &listing_id,
        OPENROUTER_SELLER_SLUG,
        &buyer_note,
        ctx.key_id.as_deref(),
    )
    .await?;
    let snap = wait_for_order_terminal(&ctx.mcp, auth, &order_id, ctx.timeout, ctx.poll).await?;
    net_key_budget_from_delivery(&ctx.mcp, ctx.key_id.as_deref(), &snap, redeemed_cents);
    let mut usage = TurnUsage::default();
    usage.add_order(&snap, redeemed_cents);
    let delivered = extract_openrouter_delivered(&snap)?;
    if delivered.error {
        return Err(OpenAiError::UpstreamFailure(delivered.text));
    }
    usage.add_tokens(&delivered);
    Ok(AgentResult {
        text: delivered.text,
        model: if delivered.model.is_empty() {
            requested_model.to_string()
        } else {
            delivered.model
        },
        order_id,
        tool_calls: delivered.tool_calls,
        usage,
    })
}

async fn buffered_chat_completion(
    ctx: &Ctx,
    req: ChatCompletionRequest,
) -> Result<ChatCompletionResponse, OpenAiError> {
    let (_npub, auth) = ctx.mcp.resolve_owned_auth()?;
    let messages = normalize_messages(&req.messages)?;
    let result = match req.client_tools() {
        Some(tools) => {
            run_passthrough_turn(
                ctx,
                &auth,
                messages,
                &req.model,
                tools,
                req.tool_choice.as_ref(),
            )
            .await?
        }
        None => run_agentic_loop(ctx, &auth, messages, &req.model).await?,
    };

    let has_calls = !result.tool_calls.is_empty();
    Ok(ChatCompletionResponse {
        id: format!("chatcmpl-{}", result.order_id),
        object: "chat.completion",
        created: unix_now(),
        model: result.model,
        choices: vec![ChatCompletionChoice {
            index: 0,
            message: ChatMessageOut {
                role: "assistant",
                content: if result.text.is_empty() && has_calls {
                    None
                } else {
                    Some(result.text)
                },
                tool_calls: has_calls.then_some(result.tool_calls),
            },
            finish_reason: if has_calls { "tool_calls" } else { "stop" },
        }],
        usage: result.usage.to_json(),
    })
}

// ---- streaming ----

fn chunk_event(id: &str, model: &str, delta: Value, finish_reason: Option<&str>) -> Event {
    let payload = json!({
        "id": format!("chatcmpl-{id}"),
        "object": "chat.completion.chunk",
        "created": unix_now(),
        "model": model,
        "choices": [{
            "index": 0,
            "delta": delta,
            "finish_reason": finish_reason,
        }],
    });
    Event::default().data(payload.to_string())
}

/// Streaming clients reassemble tool_calls by each entry's `index` field
/// (deltas may arrive fragmented, so ids alone can't group them). The
/// listing delivers whole calls, so each gets its position stamped in and
/// ships as one fragment.
fn indexed_tool_calls(calls: &[Value]) -> Vec<Value> {
    calls
        .iter()
        .enumerate()
        .map(|(i, call)| {
            let mut call = call.clone();
            if let Some(obj) = call.as_object_mut() {
                obj.entry("index").or_insert(json!(i));
            }
            call
        })
        .collect()
}

/// The turn's final `usage` frame: a chunk with **no** choices, which is
/// how OpenAI reports usage on a stream (`stream_options.include_usage`)
/// and what an OpenAI-compatible client parses without special-casing.
/// Emitted just before `[DONE]` on every successful stream, so a client
/// sees real token counts and the turn's real cost instead of having to
/// estimate from a price list it cannot know.
fn usage_event(id: &str, model: &str, usage: TurnUsage) -> Event {
    let payload = json!({
        "id": format!("chatcmpl-{id}"),
        "object": "chat.completion.chunk",
        "created": unix_now(),
        "model": model,
        "choices": [],
        "usage": usage.to_json(),
    });
    Event::default().data(payload.to_string())
}

/// The suffix of `delivered_text` not yet covered by `streamed` bytes —
/// catches up a client whose order finished before the first poll ever
/// observed a partial chunk (fast/tiny replies), or whose partial buffer
/// was truncated by the marketplace's 32 KB cap while delivered_content
/// was not. `None` once fully caught up.
///
/// A `streamed` offset landing mid-character in `delivered_text` (a
/// byte-capped partial buffer can leave one) floors to the previous
/// char boundary rather than dropping the tail: the worst case re-emits
/// the lead bytes of one already-counted character, strictly better
/// than silently losing the rest of the reply.
fn catch_up(delivered_text: &str, streamed: usize) -> Option<&str> {
    if streamed >= delivered_text.len() {
        return None;
    }
    let mut start = streamed;
    while start > 0 && !delivered_text.is_char_boundary(start) {
        start -= 1;
    }
    Some(&delivered_text[start..])
}

/// One event from following an in-flight order's streaming progress.
enum FollowEvent {
    /// Newly streamed text to forward as a content chunk.
    Delta(String),
    /// Nothing new this wakeup — emit an SSE keep-alive comment.
    KeepAlive,
    /// The order reached a terminal status: the final snapshot plus how
    /// many bytes of its partial buffer were forwarded (what `catch_up`
    /// needs to emit the uncovered tail of the delivered text).
    Terminal(Box<Value>, usize),
    /// Following failed (fetch error or the request timeout). Terminal.
    Failed(OpenAiError),
}

/// Follows one order to a terminal status, surfacing streaming progress
/// as [`FollowEvent`]s — the loop the streaming generator used to
/// duplicate per turn shape (passthrough, agentic, landing). One event
/// per `next_event` call; after `Terminal` or `Failed` the follower is
/// exhausted and must not be polled again.
///
/// Polls `GET /orders/:id?since_seq=<last>` on the `ctx.poll` cadence —
/// the marketplace omits an unchanged partial buffer for the seq it
/// already told us about — and byte-diffs the buffer exactly as before.
struct OrderFollower<'a> {
    ctx: &'a Ctx,
    auth: &'a OwnedAuth,
    order_id: &'a str,
    streamed: usize,
    last_seq: u64,
    start: Instant,
    poll_deadline: tokio::time::Instant,
    pending: Option<FollowEvent>,
    /// Live cable subscription to the order's `payment_status` topic,
    /// established lazily on the first wait when [`Ctx::ws_enabled`].
    /// While present, frames arrive as push and the poll backs off to
    /// [`Ctx::fallback_poll`] as a safety net (terminal detection is
    /// always confirmed by a GET). `None` after any failure — the
    /// follower then behaves exactly like the pure-polling version.
    ws: Option<tokio::sync::mpsc::Receiver<owallet_overpay::cable::CableFrame>>,
    ws_tried: bool,
}

impl<'a> OrderFollower<'a> {
    fn new(ctx: &'a Ctx, auth: &'a OwnedAuth, order_id: &'a str) -> Self {
        Self {
            ctx,
            auth,
            order_id,
            streamed: 0,
            last_seq: 0,
            start: Instant::now(),
            // First poll fires immediately, matching the old loops.
            poll_deadline: tokio::time::Instant::now(),
            pending: None,
            ws: None,
            ws_tried: false,
        }
    }

    async fn next_event(&mut self) -> FollowEvent {
        use owallet_overpay::cable::CableFrame;

        if let Some(ev) = self.pending.take() {
            return ev;
        }
        if self.ctx.ws_enabled && !self.ws_tried {
            self.ws_tried = true;
            // The first poll hasn't happened yet, so a failed connect
            // (bounded at 3s inside) delays nothing but itself.
            match owallet_overpay::cable::subscribe_payment_status(
                self.ctx.mcp.overpay.base_url_str(),
                self.order_id,
            )
            .await
            {
                Ok(rx) => {
                    self.ws = Some(rx);
                    tracing::debug!(order_id = self.order_id, "cable subscription live");
                }
                Err(e) => tracing::debug!(order_id = self.order_id, "cable unavailable: {e}"),
            }
        }

        loop {
            let Some(rx) = self.ws.as_mut() else {
                tokio::time::sleep_until(self.poll_deadline).await;
                return self.poll().await;
            };
            let frame = tokio::select! {
                biased;
                frame = rx.recv() => frame,
                _ = tokio::time::sleep_until(self.poll_deadline) => {
                    return self.poll().await;
                }
            };
            match frame {
                Some(CableFrame::Partial {
                    seq,
                    delta,
                    content,
                }) => {
                    // A resync frame (or an old-protocol full-buffer
                    // frame) is authoritative: run it through the same
                    // byte diff as a polled buffer.
                    if let Some(text) = content {
                        self.last_seq = self.last_seq.max(seq);
                        if let Some(new) = new_output_since(Some(&text), &mut self.streamed) {
                            return FollowEvent::Delta(new.to_string());
                        }
                        continue;
                    }
                    if seq <= self.last_seq {
                        continue; // replayed frame — already covered
                    }
                    if seq == self.last_seq + 1 {
                        if let Some(d) = delta {
                            self.last_seq = seq;
                            self.streamed += d.len();
                            return FollowEvent::Delta(d);
                        }
                        continue;
                    }
                    // Gap — a missed frame. The next conditional GET
                    // returns the full buffer (seq advanced past ours)
                    // and the byte diff emits exactly what was missed.
                    return self.poll().await;
                }
                Some(CableFrame::Refresh) => {
                    // Something changed (fulfillment transition) — poll
                    // now instead of at the next safety-net tick.
                    return self.poll().await;
                }
                Some(CableFrame::Closed) | None => {
                    self.ws = None;
                    // Back to the tight poll cadence immediately.
                    self.poll_deadline = tokio::time::Instant::now();
                }
            }
        }
    }

    /// One conditional GET: emits the new text (or a keep-alive), and
    /// stashes a terminal/timeout event for the next call — the old loop
    /// yielded its delta first and then broke/errored, so ordering is
    /// preserved exactly.
    async fn poll(&mut self) -> FollowEvent {
        let snap = match self
            .ctx
            .mcp
            .overpay
            .get_order_value_since(self.order_id, Some(self.last_seq), self.auth.as_auth())
            .await
        {
            Ok(s) => s,
            Err(e) => return FollowEvent::Failed(OpenAiError::from(e)),
        };
        let (partial, seq) = partial_output(&snap);
        self.last_seq = self.last_seq.max(seq.unwrap_or(0));
        let first = match new_output_since(partial, &mut self.streamed) {
            Some(delta) => FollowEvent::Delta(delta.to_string()),
            None => FollowEvent::KeepAlive,
        };

        if is_terminal(order_status(&snap)) {
            self.pending = Some(FollowEvent::Terminal(Box::new(snap), self.streamed));
        } else if self.start.elapsed() >= self.ctx.timeout {
            self.pending = Some(FollowEvent::Failed(OpenAiError::UpstreamFailure(format!(
                "order {} did not complete within {}s",
                self.order_id,
                self.ctx.timeout.as_secs()
            ))));
        } else {
            let interval = if self.ws.is_some() {
                self.ctx.fallback_poll
            } else {
                self.ctx.poll
            };
            self.poll_deadline = tokio::time::Instant::now() + interval;
        }
        first
    }
}

/// Terminal error frames for the streaming path: a normal-shaped
/// `chat.completion.chunk` sequence — an error-text content chunk, then a
/// `finish_reason: "stop"` chunk, then `[DONE]` — rather than a dedicated
/// out-of-band SSE event. By the time any of this runs, the HTTP status
/// and headers are already committed to 200 + `text/event-stream` (unlike
/// the buffered path, which can still return a proper error status), so
/// there is no transport-level way to signal failure here. A custom
/// `event: error` frame risks being silently dropped — or breaking a
/// parser that only understands `chat.completion.chunk`-shaped `data:`
/// frames — by a client that doesn't specifically handle it; this shape
/// guarantees the error reaches the user as visible text and ends the
/// stream exactly the way a normal completion does, so no client-side
/// special-casing is needed to notice it's over.
fn error_events(id: &str, model: &str, err: OpenAiError) -> [Event; 3] {
    let message = format!("\n\n[owallet error] {}", err.message());
    [
        chunk_event(id, model, json!({"content": message}), None),
        chunk_event(id, model, json!({}), Some("stop")),
        Event::default().data("[DONE]"),
    ]
}

/// Builds the SSE response lazily: everything inside `async_stream::stream!`
/// — resolving listings, placing and paying for each order, polling,
/// executing tool calls, yielding chunks — runs as axum drains the
/// response body, not when this function is called.
///
/// Loops over OpenRouter turns exactly like [`run_agentic_loop`] (place +
/// pay, and if the turn ends in tool_calls, execute each — a real,
/// separately-paid order per call — and go again), but streams each turn's
/// text as it's produced instead of returning it all at once. `streamed`
/// resets to 0 at the start of every new turn — `partial_content` belongs
/// to one order, not the whole conversation. A `run_python` execution
/// streams too: the `code_executor` bot publishes its own live stdout via
/// `partial_content` the same way the OpenRouter listing streams text (see
/// PR #382), so this polls and forwards it — wrapped in a markdown code
/// fence, since to the client it's just more assistant content — instead
/// of leaving the connection silent for as long as the sandbox runs. Any
/// poll (OpenRouter or Python) that has nothing new to stream sends an SSE
/// comment line instead: invisible to a client's content parsing, but
/// enough to keep an idle-timeout intermediary from thinking the
/// connection died.
///
/// `id` is fixed to the *first* OpenRouter order's id and reused for every
/// chunk of the entire response, including later internal turns and any
/// tool execution — real OpenAI-compatible clients expect one response's
/// `id` to be stable across every chunk they receive, even though this
/// endpoint's own turns are, internally, separate orders.
fn stream_chat_completion(ctx: Ctx, req: ChatCompletionRequest) -> Response {
    let requested_model = req.model.clone();
    let stream = async_stream::stream! {
        let (_npub, auth) = match ctx.mcp.resolve_owned_auth() {
            Ok(a) => a,
            Err(e) => {
                for ev in error_events("error", &requested_model, OpenAiError::from(e)) {
                    yield Ok::<_, Infallible>(ev);
                }
                return;
            }
        };
        let mut messages = match normalize_messages(&req.messages) {
            Ok(m) => m,
            Err(e) => {
                for ev in error_events("error", &requested_model, e) { yield Ok(ev); }
                return;
            }
        };
        let listing_id = match resolve_openrouter_listing_id(&ctx.mcp).await {
            Ok(id) => id,
            Err(e) => {
                for ev in error_events("error", &requested_model, e) { yield Ok(ev); }
                return;
            }
        };

        // Passthrough mode: one turn with the caller's tools, partials
        // streamed as they arrive, and any tool_calls handed back in the
        // final chunks for the caller to execute. The server-side roster
        // (and the loop below that runs it) never engages. The polling
        // shape mirrors the loop body's, per this generator's convention.
        if let Some(tools) = req.client_tools() {
            let mut buyer_note = json!({
                "model": requested_model,
                "messages": messages,
                "tools": tools,
            });
            if let Some(choice) = req.tool_choice.as_ref() {
                buyer_note["tool_choice"] = choice.clone();
            }
            let (order_id, redeemed_cents) = match place_and_pay_order(&ctx.mcp, &auth, &listing_id, OPENROUTER_SELLER_SLUG, &buyer_note, ctx.key_id.as_deref()).await {
                Ok(placed) => placed,
                Err(e) => {
                    for ev in error_events("error", &requested_model, e) { yield Ok(ev); }
                    return;
                }
            };
            yield Ok(chunk_event(&order_id, &requested_model, json!({"role": "assistant"}), None));

            let streamed;
            let mut follower = OrderFollower::new(&ctx, &auth, &order_id);
            let snap = loop {
                match follower.next_event().await {
                    FollowEvent::Delta(delta) => yield Ok(chunk_event(&order_id, &requested_model, json!({"content": delta}), None)),
                    FollowEvent::KeepAlive => yield Ok(Event::default().comment("owallet: waiting on the model")),
                    FollowEvent::Terminal(snap, streamed_bytes) => {
                        streamed = streamed_bytes;
                        break *snap;
                    }
                    FollowEvent::Failed(err) => {
                        for ev in error_events(&order_id, &requested_model, err) { yield Ok(ev); }
                        return;
                    }
                }
            };
            net_key_budget_from_delivery(&ctx.mcp, ctx.key_id.as_deref(), &snap, redeemed_cents);
            let mut usage = TurnUsage::default();
            usage.add_order(&snap, redeemed_cents);

            let delivered = match extract_openrouter_delivered(&snap) {
                Ok(d) => d,
                Err(e) => {
                    for ev in error_events(&order_id, &requested_model, e) { yield Ok(ev); }
                    return;
                }
            };
            if delivered.error {
                let err = OpenAiError::UpstreamFailure(delivered.text);
                for ev in error_events(&order_id, &requested_model, err) { yield Ok(ev); }
                return;
            }
            usage.add_tokens(&delivered);
            let model = if delivered.model.is_empty() { requested_model.clone() } else { delivered.model.clone() };
            if let Some(tail) = catch_up(&delivered.text, streamed) {
                yield Ok(chunk_event(&order_id, &model, json!({"content": tail}), None));
            }
            if delivered.tool_calls.is_empty() {
                yield Ok(chunk_event(&order_id, &model, json!({}), Some("stop")));
            } else {
                yield Ok(chunk_event(&order_id, &model, json!({"tool_calls": indexed_tool_calls(&delivered.tool_calls)}), None));
                yield Ok(chunk_event(&order_id, &model, json!({}), Some("tool_calls")));
            }
            yield Ok(usage_event(&order_id, &model, usage));
            yield Ok(Event::default().data("[DONE]"));
            return;
        }

        let python_listing_id = match resolve_python_listing_id(&ctx.mcp).await {
            Ok(id) => id,
            Err(e) => {
                for ev in error_events("error", &requested_model, e) { yield Ok(ev); }
                return;
            }
        };
        let defs = match tool_defs(&ctx, ctx.can_spend).await {
            Ok(d) => d,
            Err(e) => {
                for ev in error_events("error", &requested_model, e) { yield Ok(ev); }
                return;
            }
        };
        let mut ledger = SpendLedger::new(effective_spend_cap(&ctx));
        let mut usage = TurnUsage::default();

        let mut last_model = requested_model.clone();
        // Filled in once the first order places; every chunk after that —
        // across every turn and every tool execution — reuses it.
        let mut response_id = String::new();

        for _ in 0..MAX_TOOL_ITERATIONS {
            // Same mid-request budget break as the buffered loop.
            if exhausted_key_budget(&ctx.mcp, ctx.key_id.as_deref()).is_some() {
                break;
            }
            let buyer_note = json!({
                "model": requested_model,
                "messages": messages,
                "tools": defs.clone(),
                "tool_choice": "auto",
            });
            let (order_id, redeemed_cents) = match place_and_pay_order(&ctx.mcp, &auth, &listing_id, OPENROUTER_SELLER_SLUG, &buyer_note, ctx.key_id.as_deref()).await {
                Ok(placed) => placed,
                Err(e) => {
                    let id = if response_id.is_empty() { "error" } else { response_id.as_str() };
                    for ev in error_events(id, &last_model, e) { yield Ok(ev); }
                    return;
                }
            };
            if response_id.is_empty() {
                response_id = order_id.clone();
                yield Ok(chunk_event(&response_id, &last_model, json!({"role": "assistant"}), None));
            }

            let streamed;
            let mut follower = OrderFollower::new(&ctx, &auth, &order_id);
            let snap = loop {
                match follower.next_event().await {
                    FollowEvent::Delta(delta) => yield Ok(chunk_event(&response_id, &last_model, json!({"content": delta}), None)),
                    FollowEvent::KeepAlive => yield Ok(Event::default().comment("owallet: waiting on the model")),
                    FollowEvent::Terminal(snap, streamed_bytes) => {
                        streamed = streamed_bytes;
                        break *snap;
                    }
                    FollowEvent::Failed(err) => {
                        for ev in error_events(&response_id, &last_model, err) { yield Ok(ev); }
                        return;
                    }
                }
            };
            net_key_budget_from_delivery(&ctx.mcp, ctx.key_id.as_deref(), &snap, redeemed_cents);
            usage.add_order(&snap, redeemed_cents);

            let delivered = match extract_openrouter_delivered(&snap) {
                Ok(d) => d,
                Err(e) => {
                    for ev in error_events(&response_id, &last_model, e) { yield Ok(ev); }
                    return;
                }
            };
            if delivered.error {
                let err = OpenAiError::UpstreamFailure(delivered.text);
                for ev in error_events(&response_id, &last_model, err) { yield Ok(ev); }
                return;
            }
            if !delivered.model.is_empty() {
                last_model = delivered.model.clone();
            }
            usage.add_tokens(&delivered);

            if let Some(tail) = catch_up(&delivered.text, streamed) {
                yield Ok(chunk_event(&response_id, &last_model, json!({"content": tail}), None));
            }

            if delivered.tool_calls.is_empty() {
                yield Ok(chunk_event(&response_id, &last_model, json!({}), Some("stop")));
                yield Ok(usage_event(&response_id, &last_model, usage));
                yield Ok(Event::default().data("[DONE]"));
                return;
            }

            messages.push(json!({
                "role": "assistant",
                "content": if delivered.text.is_empty() { Value::Null } else { json!(delivered.text) },
                "tool_calls": delivered.tool_calls,
            }));

            'tool_calls: for call in &delivered.tool_calls {
                let name = call.pointer("/function/name").and_then(Value::as_str).unwrap_or_default();
                let tool_call_id = call.get("id").and_then(Value::as_str).unwrap_or_default().to_string();

                let listing_tool = if name != RUN_PYTHON_TOOL_NAME && !is_wallet_tool(name) {
                    listing_tool_named(&ctx, name).await
                } else {
                    None
                };
                if name != RUN_PYTHON_TOOL_NAME && !is_wallet_tool(name) && listing_tool.is_none() {
                    let result_text = json!({"error": format!("unknown tool '{name}'")}).to_string();
                    messages.push(json!({ "role": "tool", "tool_call_id": tool_call_id, "content": result_text }));
                    continue 'tool_calls;
                }
                let arguments_str = call.pointer("/function/arguments").and_then(Value::as_str).unwrap_or_default();
                let arguments: Value = match serde_json::from_str(arguments_str) {
                    Ok(v) => v,
                    Err(e) => {
                        let result_text = json!({"error": format!("tool call arguments are not valid JSON: {e}")}).to_string();
                        messages.push(json!({ "role": "tool", "tool_call_id": tool_call_id, "content": result_text }));
                        continue 'tool_calls;
                    }
                };

                // Wallet tools have no partial output to forward — execute,
                // record the (projected) result, and move on. An SSE comment
                // keeps idle-timeout intermediaries from hanging up while a
                // slower one (a ZEC-paid buy syncs + proves) runs.
                if is_wallet_tool(name) {
                    yield Ok(Event::default().comment("owallet: running a wallet tool"));
                    let result_text = execute_wallet_tool(&ctx.mcp, name, &arguments, ctx.can_spend, ctx.key_id.as_deref(), &mut ledger).await;
                    messages.push(json!({ "role": "tool", "tool_call_id": tool_call_id, "content": result_text }));
                    continue 'tool_calls;
                }

                // Listing tools execute like run_python (a real second
                // order), forwarding the seller's in-flight partial output
                // into the chat stream as it arrives — a streaming seller
                // (the weather reporter's forecast preview) pours into the
                // client mid-call. Unlike run_python's stdout, the preview
                // is buyer-facing markdown, so it is forwarded unfenced,
                // set off by blank lines.
                if let Some(tool) = listing_tool {
                    yield Ok(Event::default().comment(format!("owallet: running {}", tool.name)));
                    let (order_id, redeemed_cents) = match place_and_pay_order(&ctx.mcp, &auth, &tool.listing_id, &tool.seller_slug, &listing_tool_buyer_note(&tool, &arguments), ctx.key_id.as_deref()).await {
                        Ok(placed) => placed,
                        Err(e) => {
                            let result_text = json!({"error": e.message()}).to_string();
                            messages.push(json!({ "role": "tool", "tool_call_id": tool_call_id, "content": result_text }));
                            continue 'tool_calls;
                        }
                    };
                    let started = Instant::now();
                    let mut lt_streamed = 0usize;
                    let mut lt_seq = 0u64;
                    let mut lt_emitted = false;
                    let result_text = loop {
                        let snap = match ctx.mcp.overpay.get_order_value_since(&order_id, Some(lt_seq), auth.as_auth()).await {
                            Ok(s) => s,
                            Err(e) => break json!({"error": OpenAiError::from(e).message()}).to_string(),
                        };
                        lt_seq = lt_seq.max(partial_output(&snap).1.unwrap_or(0));

                        // Forward whatever is new before checking for the
                        // end, so the final flush of the preview is never
                        // dropped on the delivering poll.
                        let (partial, _seq) = partial_output(&snap);
                        match new_output_since(partial, &mut lt_streamed) {
                            Some(delta) => {
                                if !lt_emitted {
                                    yield Ok(chunk_event(&response_id, &last_model, json!({"content": "\n\n"}), None));
                                    lt_emitted = true;
                                }
                                yield Ok(chunk_event(&response_id, &last_model, json!({"content": delta}), None));
                            }
                            None => yield Ok(Event::default().comment(format!("owallet: {} still running", tool.name))),
                        }

                        if is_terminal(order_status(&snap)) {
                            net_key_budget_from_delivery(&ctx.mcp, ctx.key_id.as_deref(), &snap, redeemed_cents);
                            usage.add_order(&snap, redeemed_cents);
                            break extract_listing_delivered(&order_id, &snap).to_string();
                        }
                        if started.elapsed() >= ctx.timeout {
                            break json!({
                                "error": format!("order {order_id} did not reach a terminal status in time"),
                                "order_id": order_id,
                            }).to_string();
                        }
                        tokio::time::sleep(ctx.poll).await;
                    };
                    if lt_emitted {
                        yield Ok(chunk_event(&response_id, &last_model, json!({"content": "\n\n"}), None));
                    }
                    messages.push(json!({ "role": "tool", "tool_call_id": tool_call_id, "content": result_text }));
                    continue 'tool_calls;
                }

                let (python_order_id, py_redeemed_cents) = match place_and_pay_order(&ctx.mcp, &auth, &python_listing_id, PYTHON_SELLER_SLUG, &arguments, ctx.key_id.as_deref()).await {
                    Ok(placed) => placed,
                    Err(e) => {
                        let result_text = json!({"error": e.message()}).to_string();
                        messages.push(json!({ "role": "tool", "tool_call_id": tool_call_id, "content": result_text }));
                        continue 'tool_calls;
                    }
                };

                let mut py_streamed = 0usize;
                let mut py_seq = 0u64;
                let mut fence_open = false;
                let py_start = Instant::now();
                let python_snap;
                loop {
                    let snap = match ctx.mcp.overpay.get_order_value_since(&python_order_id, Some(py_seq), auth.as_auth()).await {
                        Ok(s) => s,
                        Err(e) => {
                            let result_text = json!({"error": OpenAiError::from(e).message()}).to_string();
                            messages.push(json!({ "role": "tool", "tool_call_id": tool_call_id, "content": result_text }));
                            continue 'tool_calls;
                        }
                    };
                    py_seq = py_seq.max(partial_output(&snap).1.unwrap_or(0));

                    let (partial, _seq) = partial_output(&snap);
                    match new_output_since(partial, &mut py_streamed) {
                        Some(delta) => {
                            if !fence_open {
                                yield Ok(chunk_event(&response_id, &last_model, json!({"content": "\n```\n"}), None));
                                fence_open = true;
                            }
                            yield Ok(chunk_event(&response_id, &last_model, json!({"content": delta}), None));
                        }
                        None => yield Ok(Event::default().comment("owallet: run_python still running")),
                    }

                    if is_terminal(order_status(&snap)) {
                        python_snap = snap;
                        break;
                    }
                    if py_start.elapsed() >= ctx.timeout {
                        let result_text = json!({"error": format!(
                            "run_python order {python_order_id} did not complete within {}s", ctx.timeout.as_secs()
                        )}).to_string();
                        messages.push(json!({ "role": "tool", "tool_call_id": tool_call_id, "content": result_text }));
                        continue 'tool_calls;
                    }
                    tokio::time::sleep(ctx.poll).await;
                }
                if fence_open {
                    yield Ok(chunk_event(&response_id, &last_model, json!({"content": "\n```\n"}), None));
                }
                net_key_budget_from_delivery(&ctx.mcp, ctx.key_id.as_deref(), &python_snap, py_redeemed_cents);
                usage.add_order(&python_snap, py_redeemed_cents);

                let result_text = match extract_python_delivered(&python_snap) {
                    Ok(result) => result.to_string(),
                    Err(e) => json!({"error": e.message()}).to_string(),
                };
                messages.push(json!({ "role": "tool", "tool_call_id": tool_call_id, "content": result_text }));
            }
        }

        // Cap reached — same landing as run_agentic_loop's: one final turn
        // with tools disabled, streamed like any other, so the client still
        // hears what actually happened (orders may already be paid).
        let buyer_note = json!({
            "model": requested_model,
            "messages": messages,
            "tools": defs.clone(),
            "tool_choice": "none",
        });
        let (order_id, redeemed_cents) = match place_and_pay_order(&ctx.mcp, &auth, &listing_id, OPENROUTER_SELLER_SLUG, &buyer_note, ctx.key_id.as_deref()).await {
            Ok(placed) => placed,
            Err(e) => {
                let id = if response_id.is_empty() { "error" } else { response_id.as_str() };
                for ev in error_events(id, &last_model, e) { yield Ok(ev); }
                return;
            }
        };
        if response_id.is_empty() {
            response_id = order_id.clone();
            yield Ok(chunk_event(&response_id, &last_model, json!({"role": "assistant"}), None));
        }
        let streamed;
        let mut follower = OrderFollower::new(&ctx, &auth, &order_id);
        let snap = loop {
            match follower.next_event().await {
                FollowEvent::Delta(delta) => yield Ok(chunk_event(&response_id, &last_model, json!({"content": delta}), None)),
                FollowEvent::KeepAlive => yield Ok(Event::default().comment("owallet: waiting on the model")),
                FollowEvent::Terminal(snap, streamed_bytes) => {
                    streamed = streamed_bytes;
                    break *snap;
                }
                FollowEvent::Failed(err) => {
                    for ev in error_events(&response_id, &last_model, err) { yield Ok(ev); }
                    return;
                }
            }
        };
        net_key_budget_from_delivery(&ctx.mcp, ctx.key_id.as_deref(), &snap, redeemed_cents);
        usage.add_order(&snap, redeemed_cents);
        let delivered = match extract_openrouter_delivered(&snap) {
            Ok(d) => d,
            Err(e) => {
                for ev in error_events(&response_id, &last_model, e) { yield Ok(ev); }
                return;
            }
        };
        if delivered.error {
            let err = OpenAiError::UpstreamFailure(delivered.text);
            for ev in error_events(&response_id, &last_model, err) { yield Ok(ev); }
            return;
        }
        if !delivered.model.is_empty() {
            last_model = delivered.model.clone();
        }
        usage.add_tokens(&delivered);
        if let Some(tail) = catch_up(&delivered.text, streamed) {
            yield Ok(chunk_event(&response_id, &last_model, json!({"content": tail}), None));
        }
        if delivered.text.is_empty() {
            let err = OpenAiError::UpstreamFailure(format!(
                "the model kept calling tools past the {MAX_TOOL_ITERATIONS}-iteration safety cap \
                 and gave no final answer even with tools disabled — stopping rather than \
                 spending further"
            ));
            for ev in error_events(&response_id, &last_model, err) { yield Ok(ev); }
            return;
        }
        yield Ok(chunk_event(&response_id, &last_model, json!({}), Some("stop")));
        yield Ok(usage_event(&response_id, &last_model, usage));
        yield Ok(Event::default().data("[DONE]"));
    };
    Sse::new(stream).into_response()
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum_test::TestServer;
    use owallet_db::Database;
    use owallet_overpay::OverpayClient;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;
    use wiremock::matchers::{body_partial_json, method, path, path_regex, query_param};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    const MODELS: &[&str] = &["openai/gpt-5-mini", "anthropic/claude-haiku-4.5"];
    const ABANDON_MNEMONIC: &str = "abandon abandon abandon abandon abandon abandon abandon \
                                     abandon abandon abandon abandon about";
    const OPENROUTER_ID: &str = "L1";
    const PYTHON_ID: &str = "PY1";

    fn openrouter_listing_body(listing_id: &str) -> Value {
        json!({"data": {
            "id": listing_id,
            "title": "OpenRouter Inference",
            "buyer_note_schema": {
                "type": "object",
                "properties": {
                    "model": {"type": "string", "enum": MODELS}
                }
            }
        }})
    }

    fn python_listing_body(listing_id: &str) -> Value {
        json!({"data": {
            "id": listing_id,
            "title": "Run Python Code",
            "description": "Run a Python 3.11 snippet in an isolated sandbox.",
            "price_usd": "$0.02",
            "free": false,
            "buyer_note_schema": {
                "type": "object",
                "required": ["code"],
                "properties": {
                    "code": {"type": "string"},
                    "stdin": {"type": "string"},
                    "requirements": {"type": "string"},
                    "requirements_lock": {"type": "string"}
                }
            }
        }})
    }

    /// Every request resolves *both* listings unconditionally (the
    /// run_python tool definition is built up front, before the first
    /// OpenRouter turn even runs) — so every completions test needs both
    /// stubbed, not just the one it's actually exercising. Query-param
    /// matching on `seller` disambiguates the two list calls; exact `path`
    /// matching on the by-id calls avoids one regex catching both ids.
    const FORECAST_ID: &str = "L-FORECAST";

    /// The forecast listing as the Rails *index* serializes it: the
    /// `provider_tool` marker rides as a curated top-level field (like
    /// `delivery_eta`), and the heavy schemas are deliberately omitted.
    fn forecast_index_row() -> Value {
        json!({
            "id": FORECAST_ID,
            "title": "AI Weather Report",
            "description": "AI-generated post-apocalyptic weather…",
            "seller": {"slug": "weather", "name": "Weather Reporter"},
            "provider_tool": {"name": "forecast"},
        })
    }

    /// The `show` payload: full description plus the buyer_note_schema —
    /// a bare-string schema (`buyer_input :text`), which the tool def
    /// must wrap in an object and unwrap at execution.
    fn forecast_show_body() -> Value {
        json!({
            "id": FORECAST_ID,
            "title": "AI Weather Report",
            "description": "AI-generated post-apocalyptic weather forecast for any location.",
            "price_usd": "$0.10",
            "free": false,
            "buyer_note_schema": {"type": "string", "title": "Enter a location"},
            "seller": {"slug": "weather", "name": "Weather Reporter"},
            "provider_tool": {"name": "forecast"},
        })
    }

    /// The unfiltered listings catalog the listing-tool registry fetches
    /// (index + the per-listing detail the registry follows up with).
    /// Mounted AFTER the seller-filtered mocks (wiremock matches in mount
    /// order), so `?seller=` lookups keep hitting their specific mocks.
    async fn mount_listing_tool_catalog(overpay: &MockServer) {
        Mock::given(method("GET"))
            .and(path("/api/v1/listings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [forecast_index_row()]
            })))
            .mount(overpay)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/api/v1/listings/{FORECAST_ID}")))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"data": forecast_show_body()})),
            )
            .mount(overpay)
            .await;
    }

    async fn mount_both_listings(overpay: &MockServer) {
        mount_seller_listing(
            overpay,
            "openrouter-bot",
            OPENROUTER_ID,
            "OpenRouter Inference",
        )
        .await;
        mount_seller_listing(overpay, "exec", PYTHON_ID, "Run Python Code").await;
        Mock::given(method("GET"))
            .and(path(format!("/api/v1/listings/{OPENROUTER_ID}")))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(openrouter_listing_body(OPENROUTER_ID)),
            )
            .mount(overpay)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/api/v1/listings/{PYTHON_ID}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(python_listing_body(PYTHON_ID)))
            .mount(overpay)
            .await;
    }

    async fn mount_seller_listing(
        overpay: &MockServer,
        seller_slug: &str,
        listing_id: &str,
        title: &str,
    ) {
        Mock::given(method("GET"))
            .and(path("/api/v1/listings"))
            .and(query_param("seller", seller_slug))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [{"id": listing_id, "title": title}]
            })))
            .mount(overpay)
            .await;
    }

    /// Two serve environments (two `McpState`s against two marketplaces)
    /// must each resolve their own listing ids. The cache used to be a
    /// pair of process-global statics, so whichever env resolved first
    /// poisoned the other with its ids under a multi-env serve.
    #[tokio::test]
    async fn listing_id_cache_is_per_state_not_process_global() {
        let overpay_a = MockServer::start().await;
        let overpay_b = MockServer::start().await;
        mount_seller_listing(
            &overpay_a,
            "openrouter-bot",
            "ENV-A",
            "OpenRouter Inference",
        )
        .await;
        mount_seller_listing(
            &overpay_b,
            "openrouter-bot",
            "ENV-B",
            "OpenRouter Inference",
        )
        .await;

        let tmp_a = TempDir::new().unwrap();
        let tmp_b = TempDir::new().unwrap();
        let state_a = seeded_state(&overpay_a.uri(), &tmp_a);
        let state_b = seeded_state(&overpay_b.uri(), &tmp_b);

        let id_a = resolve_openrouter_listing_id(&state_a)
            .await
            .unwrap_or_else(|e| panic!("env A resolve failed: {}", e.message()));
        let id_b = resolve_openrouter_listing_id(&state_b)
            .await
            .unwrap_or_else(|e| panic!("env B resolve failed: {}", e.message()));
        assert_eq!(id_a, "ENV-A");
        assert_eq!(id_b, "ENV-B");
        // And a per-request clone shares its parent's cache rather than
        // re-resolving: drop the mock so a second fetch would fail.
        drop(overpay_a);
        let pinned = state_a.with_npub(Some("npub1abandon".into()));
        let id_pinned = resolve_openrouter_listing_id(&pinned)
            .await
            .unwrap_or_else(|e| panic!("cached resolve failed: {}", e.message()));
        assert_eq!(id_pinned, "ENV-A");
    }

    async fn mount_fully_paid(overpay: &MockServer, order_id: &str) {
        mount_fully_paid_for(overpay, order_id, "openrouter-bot").await;
    }

    async fn mount_fully_paid_for(overpay: &MockServer, order_id: &str, seller_slug: &str) {
        mount_order_create(overpay, order_id).await;
        mount_redeem_fully_paid(overpay, seller_slug).await;
    }

    async fn mount_order_create(overpay: &MockServer, order_id: &str) {
        Mock::given(method("POST"))
            .and(path("/api/v1/orders"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "data": {"id": order_id, "payment_status": "pending"}
            })))
            .mount(overpay)
            .await;
    }

    async fn mount_redeem_fully_paid(overpay: &MockServer, seller_slug: &str) {
        Mock::given(method("POST"))
            .and(path(format!("/api/v1/merchant_credits/{seller_slug}/redeem")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {"status": "fully_paid", "amount_redeemed_cents": 2, "credit_balance_cents": 100}
            })))
            .mount(overpay)
            .await;
    }

    /// place_and_pay against a marketplace that settles in the create
    /// call itself: the response carries a `payment` key, and the
    /// separate redeem endpoint must never be hit (no mock is mounted
    /// for it — a call would 404 and fail the request).
    #[tokio::test]
    async fn place_and_pay_uses_the_one_call_path_when_the_marketplace_settles_on_create() {
        let overpay = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/orders"))
            .and(body_partial_json(json!({"pay": "merchant_credits"})))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "data": {"id": "ONECALL", "payment_status": "paid"},
                "payment": {"status": "fully_paid", "amount_redeemed_cents": 7}
            })))
            .mount(&overpay)
            .await;

        let tmp = TempDir::new().unwrap();
        let state = seeded_state(&overpay.uri(), &tmp);
        let (_npub, auth) = state.resolve_owned_auth().unwrap();

        let (order_id, redeemed) = place_and_pay_order(
            &state,
            &auth,
            "L1",
            "openrouter-bot",
            &json!({"model": "default"}),
            None,
        )
        .await
        .unwrap_or_else(|e| panic!("one-call path failed: {}", e.message()));
        assert_eq!(order_id, "ONECALL");
        assert_eq!(redeemed, 7);
    }

    /// A one-call redemption failure surfaces as PaymentRequired, same
    /// as the two-call path's insufficient-credits case.
    #[tokio::test]
    async fn place_and_pay_reports_a_one_call_redemption_failure() {
        let overpay = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/orders"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "data": {"id": "BROKE", "payment_status": "pending"},
                "payment": {"status": "failed", "error": "no merchant credits"}
            })))
            .mount(&overpay)
            .await;

        let tmp = TempDir::new().unwrap();
        let state = seeded_state(&overpay.uri(), &tmp);
        let (_npub, auth) = state.resolve_owned_auth().unwrap();

        let err = place_and_pay_order(
            &state,
            &auth,
            "L1",
            "openrouter-bot",
            &json!({"model": "default"}),
            None,
        )
        .await
        .expect_err("must not treat a failed payment as paid");
        assert!(
            err.message().contains("no merchant credits"),
            "got: {}",
            err.message()
        );
    }

    /// The exact `run_python` tool definition `run_python_tool_def` builds
    /// from `python_listing_body`'s fixture schema+description — every
    /// OpenRouter turn carries this in `buyer_note.tools`, so a test that
    /// asserts on the exact buyer_note string needs it too.
    fn expected_run_python_tool_def() -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "run_python",
                "description": "Run a Python 3.11 snippet in an isolated sandbox. \
                                Each call places a real marketplace order billed to the \
                                wallet (≈ $0.02 per call).",
                "parameters": {
                    "type": "object",
                    "required": ["code"],
                    "properties": {
                        "code": {"type": "string"},
                        "stdin": {"type": "string"},
                        "requirements": {"type": "string"},
                        "requirements_lock": {"type": "string"}
                    }
                }
            }
        })
    }

    /// The full tools array a chat-scoped request advertises: run_python
    /// (from the fixture listing) plus the read-only wallet tools. Spend
    /// scope appends pay_order/buy_credits — same builder the production
    /// code uses, so an exact buyer_note assertion can't silently drift.
    fn expected_tool_defs(can_spend: bool) -> Vec<Value> {
        let mut defs = vec![expected_run_python_tool_def()];
        defs.extend(wallet_tool_defs(can_spend));
        defs
    }

    fn python_delivered_content(stdout: &str, exit_code: i64) -> String {
        serde_json::to_string(&json!({
            "stdout": stdout, "stderr": "", "exit_code": exit_code,
            "duration_ms": 42, "timed_out": false
        }))
        .unwrap()
    }

    /// A realistic OpenRouter delivery. The `usage` block mirrors what the
    /// seller actually states (see the metered-settlement test below), so
    /// the turn-usage accounting is exercised by every test that delivers.
    fn delivered_content(description: &str, model: &str, error: bool) -> String {
        serde_json::to_string(&json!({
            "description": description, "model": model,
            "error": error, "credits_refunded": false,
            "usage": {"prompt_tokens": 11, "completion_tokens": 7}
        }))
        .unwrap()
    }

    /// A DB with the deterministic "abandon..." wallet seeded and selected
    /// as default — same fixture `owallet-http/tests/mcp_test.rs` uses, so
    /// `resolve_owned_auth`'s NIP-98 fallback has a real key to sign with
    /// (no bearer token is written, so every request here goes the NIP-98
    /// route rather than Bearer).
    fn seeded_state(overpay_uri: &str, tmp: &TempDir) -> McpState {
        let path = tmp.path().join("test.db");
        let db = Database::init(&path, "master-pw").unwrap();
        db.write_wallet("npub1abandon", ABANDON_MNEMONIC, Some("0xabc"))
            .unwrap();
        db.write_default_npub("npub1abandon").unwrap();
        let overpay = Arc::new(OverpayClient::new(overpay_uri).unwrap());
        McpState::new(Arc::new(Mutex::new(db)), overpay)
    }

    fn test_server(state: McpState) -> TestServer {
        test_server_with_scopes(state, "chat")
    }

    /// Same, but the key carries the given scopes — "chat spend" unlocks
    /// the wallet spending tools.
    fn test_server_with_scopes(state: McpState, scopes: &str) -> TestServer {
        test_server_with_key(state, scopes, None)
    }

    /// Same, with the key's daily budget (cents) also set.
    fn test_server_with_key(
        state: McpState,
        scopes: &str,
        daily_budget_usd_cents: Option<i64>,
    ) -> TestServer {
        let key = state
            .db
            .lock()
            .unwrap()
            .create_provider_key("npub1abandon", "test", scopes, daily_budget_usd_cents)
            .unwrap()
            .1;
        let mut server = TestServer::new(router(state)).unwrap();
        server.add_header(header::AUTHORIZATION, format!("Bearer {key}"));
        server
    }

    fn fast_test_server(state: McpState) -> TestServer {
        // Real production timing (120s/1s) would make the timeout test
        // itself time out CI. This is the one router-construction knob
        // `router_with_timing` exists for.
        let key = state
            .db
            .lock()
            .unwrap()
            .create_provider_key("npub1abandon", "test", "chat", None)
            .unwrap()
            .1;
        let app = router_with_timing(state, Duration::from_millis(150), Duration::from_millis(30));
        let mut server = TestServer::new(app).unwrap();
        server.add_header(header::AUTHORIZATION, format!("Bearer {key}"));
        server
    }

    #[tokio::test]
    async fn status_requires_a_provider_key() {
        let tmp = TempDir::new().unwrap();
        let state = seeded_state("http://127.0.0.1:1", &tmp);
        let server = TestServer::new(router(state)).unwrap();
        let response = server.get("/status").await;
        response.assert_status(StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn status_reports_projected_balances_credits_and_key_budget() {
        let overpay = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/account"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "account_number": "1234567890123456"
            })))
            .mount(&overpay)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/merchant_credits"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [{
                    "holder_type": "user", "seller_slug": "openrouter-bot",
                    "balance_cents": 480, "id": "MC1"
                }]
            })))
            .mount(&overpay)
            .await;

        let tmp = TempDir::new().unwrap();
        // EVM RPC on a dead local port: the balance read fails fast into
        // `balance_error` instead of reaching out to a real chain.
        let state = seeded_state(&overpay.uri(), &tmp)
            .with_evm("http://127.0.0.1:1".into(), "eip155:8453".into());
        let server = test_server_with_key(state, "chat", Some(500));

        let response = server.get("/status").await;
        response.assert_status_ok();
        let body: Value = response.json();

        // Chain-free: the projection must drop identifiers the raw
        // account payload carries.
        for leak in ["address", "npub", "pubkey", "zcash_address"] {
            assert!(body.get(leak).is_none(), "{leak} must not leak: {body}");
        }
        let credits = body["merchant_credits"].as_array().expect("credits");
        assert_eq!(credits[0]["seller_slug"], "openrouter-bot");
        assert_eq!(credits[0]["balance_cents"], 480);
        assert!(
            credits[0].get("id").is_none(),
            "credit ids are not projected"
        );
        assert_eq!(body["key_budget"]["daily_budget_usd"], 5.0);
        assert_eq!(body["key_budget"]["spent_today_usd"], 0.0);
        assert_eq!(body["key_budget"]["remaining_today_usd"], 5.0);
        assert!(
            body["balance_error"].is_string(),
            "dead RPC surfaces balance_error"
        );
        assert!(body["as_of"].is_string(), "as_of stamp present");
        assert_eq!(
            body["overpay_url"].as_str().expect("overpay_url"),
            overpay.uri().trim_end_matches('/'),
            "status reports the wallet's configured marketplace URL"
        );
    }

    #[test]
    fn provider_key_is_required_and_binds_the_wallet() {
        let tmp = TempDir::new().unwrap();
        let state = seeded_state("http://127.0.0.1:1", &tmp);
        let key = state
            .db
            .lock()
            .unwrap()
            .create_provider_key("npub1abandon", "test", "chat", None)
            .unwrap()
            .1;

        assert!(authenticate_provider_key(&state, &HeaderMap::new()).is_err());

        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {key}").parse().unwrap(),
        );
        let (authenticated, can_spend, key_id) = match authenticate_provider_key(&state, &headers) {
            Ok(auth) => auth,
            Err(_) => panic!("valid provider key should authenticate"),
        };
        assert_eq!(authenticated.active_npub.as_deref(), Some("npub1abandon"));
        assert!(!can_spend, "a chat-scoped key must not authorize spending");
        assert!(
            key_id.is_some(),
            "every key carries the budget handle — operating spend is accounted too"
        );

        let spend_key = state
            .db
            .lock()
            .unwrap()
            .create_provider_key("npub1abandon", "test", "chat spend", None)
            .unwrap()
            .1;
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {spend_key}").parse().unwrap(),
        );
        let Ok((_, can_spend, key_id)) = authenticate_provider_key(&state, &headers) else {
            panic!("valid spend-scoped key should authenticate");
        };
        assert!(can_spend);
        assert!(key_id.is_some());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn models_reads_the_listings_own_curated_enum() {
        let overpay = MockServer::start().await;
        mount_both_listings(&overpay).await;
        let tmp = TempDir::new().unwrap();
        let s = test_server(seeded_state(&overpay.uri(), &tmp));

        let res = s.get("/models").await;
        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["object"], "list");
        let ids: Vec<String> = body["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            ids[0], DEFAULT_MODEL,
            "the sentinel is always listed first: {ids:?}"
        );
        assert_eq!(&ids[1..], MODELS);
        assert_eq!(body["data"][0]["object"], "model");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn validate_request_accepts_the_default_sentinel_without_touching_overpay() {
        // No mocks registered at all -- validate_request must not need a
        // live catalog fetch to accept `DEFAULT_MODEL`, since the whole
        // point is that it works even when the listing can't be reached
        // (e.g. right after `owallet install` wrote a fallback provider
        // entry with only this model, per its own doc comment).
        let overpay = MockServer::start().await;
        let tmp = TempDir::new().unwrap();
        let state = seeded_state(&overpay.uri(), &tmp);
        let req = ChatCompletionRequest {
            model: DEFAULT_MODEL.to_string(),
            messages: vec![json!({"role": "user", "content": "hi"})],
            stream: false,
            tools: None,
            tool_choice: None,
        };
        assert!(validate_request(&state, &req).await.is_ok());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn chat_completions_forwards_the_default_sentinel_to_the_listing_as_is() {
        // The listing's own `coerce_model` (Ruby) is what actually resolves
        // "default" to a real model id and calls OpenRouter with it -- this
        // endpoint's job is only to forward the literal string unchanged,
        // same as any other model value, which this asserts directly on
        // the buyer_note the order carries.
        let overpay = MockServer::start().await;
        mount_both_listings(&overpay).await;

        let expected_note = serde_json::to_string(&json!({
            "model": DEFAULT_MODEL,
            "messages": [{"role": "user", "content": "hi"}],
            "tools": expected_tool_defs(false),
            "tool_choice": "auto"
        }))
        .unwrap();
        Mock::given(method("POST"))
            .and(path("/api/v1/orders"))
            .and(body_partial_json(json!({
                "listing_id": "L1",
                "buyer_note": expected_note
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "data": {"id": "OD", "payment_status": "pending"}
            })))
            .expect(1)
            .mount(&overpay)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/merchant_credits/openrouter-bot/redeem"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {"status": "fully_paid", "amount_redeemed_cents": 2, "credit_balance_cents": 100}
            })))
            .mount(&overpay)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/orders/OD"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "OD", "fulfillment_status": "delivered",
                    // Reports back whatever real model the listing actually
                    // resolved "default" to -- this endpoint doesn't need
                    // to know or care what that was.
                    "delivered_content": delivered_content("hi there", "openai/gpt-5-mini", false),
                }
            })))
            .mount(&overpay)
            .await;

        let tmp = TempDir::new().unwrap();
        let s = test_server(seeded_state(&overpay.uri(), &tmp));

        let res = s
            .post("/chat/completions")
            .json(&json!({
                "model": DEFAULT_MODEL,
                "messages": [{"role": "user", "content": "hi"}],
            }))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["choices"][0]["message"]["content"], "hi there");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn chat_completions_rejects_a_model_outside_the_safelist() {
        let overpay = MockServer::start().await;
        mount_both_listings(&overpay).await;
        let tmp = TempDir::new().unwrap();
        let s = test_server(seeded_state(&overpay.uri(), &tmp));

        let res = s
            .post("/chat/completions")
            .json(&json!({
                "model": "not/a-real-model",
                "messages": [{"role": "user", "content": "hi"}],
            }))
            .await;

        res.assert_status(StatusCode::BAD_REQUEST);
        let body: Value = res.json();
        assert_eq!(body["error"]["type"], "invalid_request_error");
        assert!(body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not/a-real-model"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn chat_completions_rejects_empty_messages() {
        let overpay = MockServer::start().await;
        mount_both_listings(&overpay).await;
        let tmp = TempDir::new().unwrap();
        let s = test_server(seeded_state(&overpay.uri(), &tmp));

        let res = s
            .post("/chat/completions")
            .json(&json!({"model": "openai/gpt-5-mini", "messages": []}))
            .await;

        res.assert_status(StatusCode::BAD_REQUEST);
        let body: Value = res.json();
        assert_eq!(body["error"]["type"], "invalid_request_error");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn chat_completions_buffered_happy_path_forwards_the_full_message_history() {
        let overpay = MockServer::start().await;
        mount_both_listings(&overpay).await;

        // Asserts the exact buyer_note the order carries: the full
        // messages array (not a flattened prompt), JSON-encoded as a
        // string per create_order's Python-parity wire shape.
        let expected_note = serde_json::to_string(&json!({
            "model": "openai/gpt-5-mini",
            "messages": [
                {"role": "system", "content": "be terse"},
                {"role": "user", "content": "say hi"}
            ],
            "tools": expected_tool_defs(false),
            "tool_choice": "auto"
        }))
        .unwrap();
        Mock::given(method("POST"))
            .and(path("/api/v1/orders"))
            .and(body_partial_json(json!({
                "listing_id": "L1",
                "buyer_note": expected_note
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "data": {"id": "O1", "payment_status": "pending"}
            })))
            .expect(1)
            .mount(&overpay)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/merchant_credits/openrouter-bot/redeem"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {"status": "fully_paid", "amount_redeemed_cents": 2, "credit_balance_cents": 100}
            })))
            .mount(&overpay)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/orders/O1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "O1", "fulfillment_status": "delivered",
                    "delivered_content": delivered_content("Hello!", "openai/gpt-5-mini", false),
                }
            })))
            .mount(&overpay)
            .await;

        let tmp = TempDir::new().unwrap();
        let s = test_server(seeded_state(&overpay.uri(), &tmp));

        let res = s
            .post("/chat/completions")
            .json(&json!({
                "model": "openai/gpt-5-mini",
                "messages": [
                    {"role": "system", "content": "be terse"},
                    {"role": "user", "content": "say hi"}
                ],
            }))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["object"], "chat.completion");
        assert_eq!(body["model"], "openai/gpt-5-mini");
        assert!(body["id"].as_str().unwrap().starts_with("chatcmpl-"));
        assert_eq!(body["choices"][0]["message"]["role"], "assistant");
        assert_eq!(body["choices"][0]["message"]["content"], "Hello!");
        assert_eq!(body["choices"][0]["finish_reason"], "stop");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn chat_completions_insufficient_credits_returns_402() {
        let overpay = MockServer::start().await;
        mount_both_listings(&overpay).await;
        Mock::given(method("POST"))
            .and(path("/api/v1/orders"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "data": {"id": "O2", "payment_status": "pending"}
            })))
            .mount(&overpay)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/merchant_credits/openrouter-bot/redeem"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {"status": "partial", "amount_redeemed_cents": 0,
                         "credit_balance_cents": 0, "message": "No available credits"}
            })))
            .mount(&overpay)
            .await;

        let tmp = TempDir::new().unwrap();
        let s = test_server(seeded_state(&overpay.uri(), &tmp));

        let res = s
            .post("/chat/completions")
            .json(&json!({
                "model": "openai/gpt-5-mini",
                "messages": [{"role": "user", "content": "hi"}],
            }))
            .await;

        res.assert_status(StatusCode::PAYMENT_REQUIRED);
        let body: Value = res.json();
        assert_eq!(body["error"]["type"], "insufficient_quota");
        assert!(body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("load_core_credits"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn chat_completions_surfaces_a_delivered_upstream_error_as_a_real_error() {
        // The Ruby listing's deliver_upstream_error path marks the order
        // `delivered` (not `failed`) with `error: true` in the payload —
        // this must not come back to the client as if it were the
        // assistant's answer.
        let overpay = MockServer::start().await;
        mount_both_listings(&overpay).await;
        mount_fully_paid(&overpay, "O3").await;
        Mock::given(method("GET"))
            .and(path("/api/v1/orders/O3"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "O3", "fulfillment_status": "delivered",
                    "delivered_content": delivered_content(
                        "OpenRouter could not serve this request: model deprecated",
                        "openai/gpt-5-mini", true
                    ),
                }
            })))
            .mount(&overpay)
            .await;

        let tmp = TempDir::new().unwrap();
        let s = test_server(seeded_state(&overpay.uri(), &tmp));

        let res = s
            .post("/chat/completions")
            .json(&json!({
                "model": "openai/gpt-5-mini",
                "messages": [{"role": "user", "content": "hi"}],
            }))
            .await;

        res.assert_status(StatusCode::BAD_GATEWAY);
        let body: Value = res.json();
        assert!(body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("model deprecated"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn chat_completions_times_out_on_a_stuck_order() {
        let overpay = MockServer::start().await;
        mount_both_listings(&overpay).await;
        mount_fully_paid(&overpay, "O4").await;
        Mock::given(method("GET"))
            .and(path("/api/v1/orders/O4"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {"id": "O4", "fulfillment_status": "awaiting_seller"}
            })))
            .mount(&overpay)
            .await;

        let tmp = TempDir::new().unwrap();
        let s = fast_test_server(seeded_state(&overpay.uri(), &tmp));

        let res = s
            .post("/chat/completions")
            .json(&json!({
                "model": "openai/gpt-5-mini",
                "messages": [{"role": "user", "content": "hi"}],
            }))
            .await;

        res.assert_status(StatusCode::BAD_GATEWAY);
        let body: Value = res.json();
        assert!(body["error"]["message"].as_str().unwrap().contains("O4"));
    }

    /// A seller's buffer growing across polls, then delivering — mirrors
    /// `mcp_wait_for_order_streams_seller_output_as_deltas` in
    /// `owallet-http/tests/mcp_test.rs`, adapted to this endpoint's own
    /// polling loop and OpenAI chunk shape.
    struct GrowingBuffer {
        calls: AtomicUsize,
    }
    impl Respond for GrowingBuffer {
        fn respond(&self, _req: &Request) -> ResponseTemplate {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let body = match n {
                0 => json!({"data": {
                    "id": "O5", "fulfillment_status": "processing",
                    "partial_content": "Four score", "partial_seq": 1,
                }}),
                1 => json!({"data": {
                    "id": "O5", "fulfillment_status": "processing",
                    "partial_content": "Four score and seven", "partial_seq": 2,
                }}),
                // Delivered text matches the last partial exactly — the
                // real invariant for this listing (stream() and the text
                // it accumulates happen in the same loop). The mismatched
                // case (delivered text longer than anything ever streamed)
                // is its own dedicated test, below.
                _ => json!({"data": {
                    "id": "O5", "fulfillment_status": "delivered",
                    "delivered_content": delivered_content(
                        "Four score and seven", "openai/gpt-5-mini", false
                    ),
                }}),
            };
            ResponseTemplate::new(200).set_body_json(body)
        }
    }

    /// A mock marketplace whose /cable route is a REAL WebSocket speaking
    /// the ActionCable protocol, and whose order endpoint never returns
    /// any partial content — so a streamed delta in the SSE output can
    /// only have arrived by push. The cable handler flips the order to
    /// delivered right before sending its refresh frame.
    async fn ws_marketplace() -> String {
        use axum::extract::ws::{Message as WsMessage, WebSocketUpgrade};
        use axum::extract::{Path as AxPath, Query, State as AxState};
        use axum::response::IntoResponse;
        use axum::routing::{get as ax_get, post as ax_post};
        use std::collections::HashMap;
        use std::sync::atomic::AtomicBool;

        type Delivered = Arc<AtomicBool>;

        async fn listings(Query(q): Query<HashMap<String, String>>) -> axum::Json<Value> {
            let data = match q.get("seller").map(String::as_str) {
                Some("openrouter-bot") => {
                    json!([{"id": OPENROUTER_ID, "title": "OpenRouter Inference"}])
                }
                Some("exec") => json!([{"id": PYTHON_ID, "title": "Run Python Code"}]),
                _ => json!([]),
            };
            axum::Json(json!({ "data": data }))
        }
        async fn listing_show(AxPath(id): AxPath<String>) -> axum::Json<Value> {
            if id == PYTHON_ID {
                axum::Json(python_listing_body(PYTHON_ID))
            } else {
                axum::Json(openrouter_listing_body(OPENROUTER_ID))
            }
        }
        async fn create_order() -> axum::Json<Value> {
            axum::Json(json!({"data": {"id": "WS-ORDER"}}))
        }
        async fn redeem() -> axum::Json<Value> {
            axum::Json(json!({"data": {"status": "fully_paid", "amount_redeemed_cents": 100}}))
        }
        async fn order_show(AxState(delivered): AxState<Delivered>) -> axum::Json<Value> {
            if delivered.load(Ordering::SeqCst) {
                axum::Json(json!({"data": {
                    "id": "WS-ORDER",
                    "fulfillment_status": "delivered",
                    "delivered_content": delivered_content("Hello world", "m1", false),
                }}))
            } else {
                axum::Json(json!({"data": {"id": "WS-ORDER", "fulfillment_status": "processing"}}))
            }
        }
        async fn cable(
            ws: WebSocketUpgrade,
            AxState(delivered): AxState<Delivered>,
        ) -> impl IntoResponse {
            ws.on_upgrade(move |mut socket| async move {
                let send = |v: Value| WsMessage::Text(v.to_string().into());
                let _ = socket.send(send(json!({"type": "welcome"}))).await;
                while let Some(Ok(msg)) = socket.recv().await {
                    if let WsMessage::Text(t) = msg {
                        let v: Value = serde_json::from_str(&t).unwrap_or_default();
                        if v["command"] == "subscribe" {
                            break;
                        }
                    }
                }
                let _ = socket
                    .send(send(json!({"type": "confirm_subscription"})))
                    .await;
                let _ = socket
                    .send(send(json!({"identifier": "i", "message":
                        {"action": "partial", "seq": 1, "delta": "Hel", "content": "Hel"}})))
                    .await;
                let _ = socket
                    .send(send(json!({"identifier": "i", "message":
                        {"action": "partial", "seq": 2, "delta": "lo "}})))
                    .await;
                delivered.store(true, Ordering::SeqCst);
                let _ = socket
                    .send(send(
                        json!({"identifier": "i", "message": {"action": "refresh"}}),
                    ))
                    .await;
                // Hold the socket open past the request's lifetime.
                tokio::time::sleep(Duration::from_secs(5)).await;
            })
        }

        let delivered: Delivered = Arc::new(AtomicBool::new(false));
        let app = axum::Router::new()
            .route("/api/v1/listings", ax_get(listings))
            .route("/api/v1/listings/{id}", ax_get(listing_show))
            .route("/api/v1/orders", ax_post(create_order))
            .route("/api/v1/orders/{id}", ax_get(order_show))
            .route("/api/v1/merchant_credits/{slug}/redeem", ax_post(redeem))
            .route("/cable", ax_get(cable))
            .with_state(delivered);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn streaming_pushes_deltas_over_the_cable_socket() {
        let base = ws_marketplace().await;
        let tmp = TempDir::new().unwrap();
        let state = seeded_state(&base, &tmp);
        let key = state
            .db
            .lock()
            .unwrap()
            .create_provider_key("npub1abandon", "test", "chat", None)
            .unwrap()
            .1;
        let app = router_with_config_full(
            state,
            Duration::from_secs(10),
            Duration::from_millis(100),
            20.0,
            true,                       // ws on — the point of this test
            Duration::from_millis(300), // safety-net poll while ws is live
        );
        let mut server = TestServer::new(app).unwrap();
        server.add_header(header::AUTHORIZATION, format!("Bearer {key}"));

        let res = server
            .post("/chat/completions")
            .json(&json!({
                "model": "default",
                "messages": [{"role": "user", "content": "hi"}],
                "stream": true,
            }))
            .await;

        res.assert_status_ok();
        let text = res.text();
        let content: String = text
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(str::trim)
            .filter(|l| *l != "[DONE]")
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter_map(|v| {
                v["choices"][0]["delta"]["content"]
                    .as_str()
                    .map(str::to_string)
            })
            .collect();

        // "Hel" + "lo " can only have arrived over the socket (the order
        // endpoint never serves partial_content); "world" is the terminal
        // catch_up tail from delivered_content.
        assert_eq!(content, "Hello world", "full SSE text was: {text}");
        assert!(text.contains("[DONE]"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn chat_completions_streaming_emits_incremental_deltas_then_done() {
        let overpay = MockServer::start().await;
        mount_both_listings(&overpay).await;
        mount_fully_paid(&overpay, "O5").await;
        Mock::given(method("GET"))
            .and(path("/api/v1/orders/O5"))
            .respond_with(GrowingBuffer {
                calls: AtomicUsize::new(0),
            })
            .mount(&overpay)
            .await;

        let tmp = TempDir::new().unwrap();
        let s = test_server(seeded_state(&overpay.uri(), &tmp));

        let res = s
            .post("/chat/completions")
            .json(&json!({
                "model": "openai/gpt-5-mini",
                "messages": [{"role": "user", "content": "hi"}],
                "stream": true,
            }))
            .await;

        res.assert_status_ok();
        let text = res.text();

        let deltas: Vec<String> = text
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(str::trim)
            .filter(|l| *l != "[DONE]")
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter_map(|v| {
                v["choices"][0]["delta"]["content"]
                    .as_str()
                    .map(str::to_string)
            })
            .collect();

        assert_eq!(
            deltas,
            vec!["Four score".to_string(), " and seven".to_string()],
            "each chunk should carry only the newly generated text:\n{text}"
        );
        assert!(text.trim_end().ends_with("data: [DONE]"), "stream: {text}");

        // The last chunk that carries choices ends the turn; the usage
        // frame that follows it deliberately has none (OpenAI's
        // include_usage shape), so it is excluded here and asserted below.
        let finish_reasons: Vec<Value> = text
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(str::trim)
            .filter(|l| *l != "[DONE]")
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|v| !v["choices"].as_array().is_none_or(|c| c.is_empty()))
            .map(|v| v["choices"][0]["finish_reason"].clone())
            .collect();
        assert_eq!(
            finish_reasons.last(),
            Some(&Value::String("stop".to_string())),
            "final content chunk must carry finish_reason=stop: {text}"
        );

        let usage = last_stream_usage(&text).expect("stream must end with a usage frame");
        assert_eq!(usage["prompt_tokens"], json!(11), "usage: {text}");
        assert_eq!(usage["completion_tokens"], json!(7), "usage: {text}");
        assert_eq!(usage["total_tokens"], json!(18), "usage: {text}");
    }

    /// The `usage` block off the last usage-bearing frame of an SSE body.
    fn last_stream_usage(text: &str) -> Option<Value> {
        text.lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(str::trim)
            .filter(|l| *l != "[DONE]")
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter_map(|v| v.get("usage").cloned())
            .next_back()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn chat_completions_streaming_catches_up_a_reply_that_finished_before_the_first_poll() {
        // No partial_content was ever observed -- the order is already
        // `delivered` on the very first poll. The full text must still
        // reach the client as one chunk, not be silently dropped.
        let overpay = MockServer::start().await;
        mount_both_listings(&overpay).await;
        mount_fully_paid(&overpay, "O6").await;
        Mock::given(method("GET"))
            .and(path("/api/v1/orders/O6"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "O6", "fulfillment_status": "delivered",
                    "delivered_content": delivered_content("instant reply", "openai/gpt-5-mini", false),
                }
            })))
            .mount(&overpay)
            .await;

        let tmp = TempDir::new().unwrap();
        let s = test_server(seeded_state(&overpay.uri(), &tmp));

        let res = s
            .post("/chat/completions")
            .json(&json!({
                "model": "openai/gpt-5-mini",
                "messages": [{"role": "user", "content": "hi"}],
                "stream": true,
            }))
            .await;

        res.assert_status_ok();
        let text = res.text();
        let deltas: Vec<String> = text
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(str::trim)
            .filter(|l| *l != "[DONE]")
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter_map(|v| {
                v["choices"][0]["delta"]["content"]
                    .as_str()
                    .map(str::to_string)
            })
            .collect();
        assert_eq!(deltas, vec!["instant reply".to_string()], "stream: {text}");
    }

    // ---- tool-calling: run_python executed server-side ----

    fn delivered_content_with_tool_call(
        model: &str,
        call_id: &str,
        tool_name: &str,
        arguments: &str,
    ) -> String {
        serde_json::to_string(&json!({
            "description": "", "model": model, "error": false, "credits_refunded": false,
            "tool_calls": [{
                "id": call_id, "type": "function",
                "function": { "name": tool_name, "arguments": arguments }
            }]
        }))
        .unwrap()
    }

    /// Routes `POST /api/v1/orders` to a fresh, incrementing order id per
    /// listing (`OR-0`, `OR-1`, … for OpenRouter; `PY-0`, `PY-1`, … for the
    /// Python listing) based on the request's `listing_id` — so a test
    /// spanning multiple OpenRouter turns and tool executions can mount a
    /// distinct, deterministic GET response for each one.
    #[derive(Default)]
    struct OrderCreateRouter {
        openrouter_calls: AtomicUsize,
        python_calls: AtomicUsize,
        forecast_calls: AtomicUsize,
        /// The buyer_note of the last forecast order, for verbatim-string
        /// assertions (a `buyer_input :text` note must arrive unquoted).
        forecast_note: std::sync::Arc<std::sync::Mutex<Option<Value>>>,
        /// The buyer_note of the last OpenRouter order — passthrough tests
        /// assert the caller's tools rode it verbatim.
        openrouter_note: std::sync::Arc<std::sync::Mutex<Option<Value>>>,
    }
    impl Respond for OrderCreateRouter {
        fn respond(&self, req: &Request) -> ResponseTemplate {
            let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
            let listing_id = body
                .get("listing_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let id = if listing_id == OPENROUTER_ID {
                *self.openrouter_note.lock().unwrap() = body.get("buyer_note").cloned();
                format!(
                    "OR-{}",
                    self.openrouter_calls.fetch_add(1, Ordering::SeqCst)
                )
            } else if listing_id == PYTHON_ID {
                format!("PY-{}", self.python_calls.fetch_add(1, Ordering::SeqCst))
            } else if listing_id == FORECAST_ID {
                *self.forecast_note.lock().unwrap() = body.get("buyer_note").cloned();
                format!("F-{}", self.forecast_calls.fetch_add(1, Ordering::SeqCst))
            } else {
                panic!("unexpected listing_id in create_order body: {body}");
            };
            ResponseTemplate::new(201)
                .set_body_json(json!({"data": {"id": id, "payment_status": "pending"}}))
        }
    }

    async fn mount_order_router(overpay: &MockServer) {
        mount_order_router_capturing(overpay).await;
    }

    /// Like [`mount_order_router`] but hands back the forecast buyer_note
    /// capture slot.
    async fn mount_order_router_capturing(
        overpay: &MockServer,
    ) -> std::sync::Arc<std::sync::Mutex<Option<Value>>> {
        let note = std::sync::Arc::new(std::sync::Mutex::new(None));
        Mock::given(method("POST"))
            .and(path("/api/v1/orders"))
            .respond_with(OrderCreateRouter {
                forecast_note: note.clone(),
                ..Default::default()
            })
            .mount(overpay)
            .await;
        mount_redeem_fully_paid(overpay, "openrouter-bot").await;
        mount_redeem_fully_paid(overpay, "exec").await;
        note
    }

    /// Like [`mount_order_router`] but hands back the last OpenRouter
    /// order's buyer_note (a JSON-encoded string — parse before asserting).
    async fn mount_order_router_capturing_openrouter(
        overpay: &MockServer,
    ) -> std::sync::Arc<std::sync::Mutex<Option<Value>>> {
        let note = std::sync::Arc::new(std::sync::Mutex::new(None));
        Mock::given(method("POST"))
            .and(path("/api/v1/orders"))
            .respond_with(OrderCreateRouter {
                openrouter_note: note.clone(),
                ..Default::default()
            })
            .mount(overpay)
            .await;
        mount_redeem_fully_paid(overpay, "openrouter-bot").await;
        mount_redeem_fully_paid(overpay, "exec").await;
        note
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn buffered_chat_completion_executes_a_tool_call_then_returns_the_final_answer() {
        let overpay = MockServer::start().await;
        mount_both_listings(&overpay).await;
        mount_order_router(&overpay).await;

        // Turn 1: the model calls run_python instead of answering directly.
        Mock::given(method("GET"))
            .and(path("/api/v1/orders/OR-0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "OR-0", "fulfillment_status": "delivered",
                    "delivered_content": delivered_content_with_tool_call(
                        "openai/gpt-5-mini", "call_1", "run_python", r#"{"code": "print(1+1)"}"#
                    ),
                }
            })))
            .mount(&overpay)
            .await;
        // The tool itself: a real, separate order against the Python listing.
        Mock::given(method("GET"))
            .and(path("/api/v1/orders/PY-0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "PY-0", "fulfillment_status": "delivered",
                    "delivered_content": python_delivered_content("2\n", 0),
                }
            })))
            .mount(&overpay)
            .await;
        // Turn 2: given the tool's result, the model answers for real.
        Mock::given(method("GET"))
            .and(path("/api/v1/orders/OR-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "OR-1", "fulfillment_status": "delivered",
                    "delivered_content": delivered_content("The answer is 2.", "openai/gpt-5-mini", false),
                }
            })))
            .mount(&overpay)
            .await;

        let tmp = TempDir::new().unwrap();
        let s = test_server(seeded_state(&overpay.uri(), &tmp));

        let res = s
            .post("/chat/completions")
            .json(&json!({
                "model": "openai/gpt-5-mini",
                "messages": [{"role": "user", "content": "what is 1+1?"}],
            }))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["choices"][0]["message"]["content"], "The answer is 2.");
        assert_eq!(body["choices"][0]["finish_reason"], "stop");
        // The chatcmpl id is the *last* OpenRouter order's id -- the turn
        // that actually produced the answer -- not the first (tool-calling)
        // turn.
        assert_eq!(body["id"], "chatcmpl-OR-1");

        // Usage covers the whole turn, not just the turn that answered:
        // three real orders were placed and paid (two OpenRouter turns plus
        // the run_python order), each redeeming the mock's 2¢. A tool order
        // carries no tokens of its own, so only the two model turns'
        // token counts land.
        assert_eq!(body["usage"]["charged_cents"], json!(6), "body: {body}");
        assert_eq!(body["usage"]["cost"], json!(0.06), "body: {body}");
        assert_eq!(body["usage"]["prompt_tokens"], json!(11), "body: {body}");
        assert_eq!(body["usage"]["completion_tokens"], json!(7), "body: {body}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_turn_that_pays_nothing_reports_zero_rather_than_guessing() {
        // No delivery states `charged_cents` and the mock redeems nothing
        // meaningful, so the endpoint reports what it knows instead of
        // inventing a token-price estimate the wallet never paid.
        let overpay = MockServer::start().await;
        mount_both_listings(&overpay).await;
        mount_order_router(&overpay).await;
        Mock::given(method("GET"))
            .and(path("/api/v1/orders/OR-0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "OR-0", "fulfillment_status": "delivered",
                    "delivered_content": serde_json::to_string(&json!({
                        "description": "Hi.", "model": "openai/gpt-5-mini",
                        "error": false, "credits_refunded": false,
                    }))
                    .unwrap(),
                }
            })))
            .mount(&overpay)
            .await;

        let tmp = TempDir::new().unwrap();
        let s = test_server(seeded_state(&overpay.uri(), &tmp));

        let res = s
            .post("/chat/completions")
            .json(&json!({
                "model": "openai/gpt-5-mini",
                "messages": [{"role": "user", "content": "hi"}],
            }))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["usage"]["prompt_tokens"], json!(0), "body: {body}");
        assert_eq!(body["usage"]["completion_tokens"], json!(0), "body: {body}");
        assert_eq!(body["usage"]["total_tokens"], json!(0), "body: {body}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn passthrough_returns_client_tool_calls_instead_of_executing() {
        let overpay = MockServer::start().await;
        mount_both_listings(&overpay).await;
        let note = mount_order_router_capturing_openrouter(&overpay).await;

        // The model calls the *caller's* tool. In server mode this name
        // would be rejected as unknown and looped on; in passthrough it
        // must come back unexecuted — as the only order placed.
        Mock::given(method("GET"))
            .and(path("/api/v1/orders/OR-0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "OR-0", "fulfillment_status": "delivered",
                    "delivered_content": delivered_content_with_tool_call(
                        "openai/gpt-5-mini", "call_ld", "list_dir", r#"{"path": "."}"#
                    ),
                }
            })))
            .mount(&overpay)
            .await;

        let client_tools = json!([{
            "type": "function",
            "function": {
                "name": "list_dir",
                "description": "List a directory",
                "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}
            }
        }]);

        let tmp = TempDir::new().unwrap();
        let s = test_server(seeded_state(&overpay.uri(), &tmp));
        let res = s
            .post("/chat/completions")
            .json(&json!({
                "model": "openai/gpt-5-mini",
                "messages": [{"role": "user", "content": "what's in the cwd?"}],
                "tools": client_tools,
                "tool_choice": "auto",
            }))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["choices"][0]["finish_reason"], "tool_calls");
        let message = &body["choices"][0]["message"];
        assert_eq!(
            message["content"],
            Value::Null,
            "pure tool-call turn: {body}"
        );
        assert_eq!(message["tool_calls"][0]["id"], "call_ld");
        assert_eq!(message["tool_calls"][0]["function"]["name"], "list_dir");
        // One order, unexecuted — the id proves no second turn ran.
        assert_eq!(body["id"], "chatcmpl-OR-0");

        // The caller's definitions rode the buyer_note verbatim; the
        // server roster did not.
        let captured = note.lock().unwrap().clone().expect("buyer_note captured");
        let inner: Value = serde_json::from_str(captured.as_str().unwrap()).unwrap();
        assert_eq!(inner["tools"], client_tools);
        assert_eq!(inner["tool_choice"], "auto");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn passthrough_round_trips_tool_results_to_a_final_answer() {
        let overpay = MockServer::start().await;
        mount_both_listings(&overpay).await;
        let note = mount_order_router_capturing_openrouter(&overpay).await;

        Mock::given(method("GET"))
            .and(path("/api/v1/orders/OR-0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "OR-0", "fulfillment_status": "delivered",
                    "delivered_content": delivered_content("Two files: a and b.", "openai/gpt-5-mini", false),
                }
            })))
            .mount(&overpay)
            .await;

        let tmp = TempDir::new().unwrap();
        let s = test_server(seeded_state(&overpay.uri(), &tmp));
        // The continuation request an OpenAI client sends after executing
        // the call: its history carries the assistant tool_calls turn and
        // the tool result.
        let res = s
            .post("/chat/completions")
            .json(&json!({
                "model": "openai/gpt-5-mini",
                "messages": [
                    {"role": "user", "content": "what's in the cwd?"},
                    {"role": "assistant", "content": null, "tool_calls": [{
                        "id": "call_ld", "type": "function",
                        "function": {"name": "list_dir", "arguments": "{\"path\": \".\"}"}
                    }]},
                    {"role": "tool", "tool_call_id": "call_ld", "content": "a\nb\n"},
                ],
                "tools": [{"type": "function", "function": {"name": "list_dir", "parameters": {}}}],
            }))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["choices"][0]["finish_reason"], "stop");
        assert_eq!(
            body["choices"][0]["message"]["content"],
            "Two files: a and b."
        );
        assert!(body["choices"][0]["message"].get("tool_calls").is_none());

        // The tool round survived normalization into the buyer_note.
        let captured = note.lock().unwrap().clone().expect("buyer_note captured");
        let inner: Value = serde_json::from_str(captured.as_str().unwrap()).unwrap();
        let messages = inner["messages"].as_array().unwrap();
        assert_eq!(messages[1]["tool_calls"][0]["id"], "call_ld");
        assert_eq!(messages[2]["role"], "tool");
        assert_eq!(messages[2]["tool_call_id"], "call_ld");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_empty_tools_array_stays_in_server_mode() {
        let overpay = MockServer::start().await;
        mount_both_listings(&overpay).await;
        let note = mount_order_router_capturing_openrouter(&overpay).await;

        Mock::given(method("GET"))
            .and(path("/api/v1/orders/OR-0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "OR-0", "fulfillment_status": "delivered",
                    "delivered_content": delivered_content("Hi.", "openai/gpt-5-mini", false),
                }
            })))
            .mount(&overpay)
            .await;

        let tmp = TempDir::new().unwrap();
        let s = test_server(seeded_state(&overpay.uri(), &tmp));
        let res = s
            .post("/chat/completions")
            .json(&json!({
                "model": "openai/gpt-5-mini",
                "messages": [{"role": "user", "content": "hi"}],
                "tools": [],
            }))
            .await;

        res.assert_status_ok();
        // Server mode advertised its own roster despite the empty array —
        // some SDKs send `tools: []` for plain chat.
        let captured = note.lock().unwrap().clone().expect("buyer_note captured");
        let inner: Value = serde_json::from_str(captured.as_str().unwrap()).unwrap();
        let names: Vec<&str> = inner["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t.pointer("/function/name").and_then(Value::as_str))
            .collect();
        assert!(
            names.contains(&"run_python"),
            "server roster expected: {names:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn streaming_passthrough_hands_back_tool_call_chunks() {
        let overpay = MockServer::start().await;
        mount_both_listings(&overpay).await;
        mount_order_router(&overpay).await;

        Mock::given(method("GET"))
            .and(path("/api/v1/orders/OR-0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "OR-0", "fulfillment_status": "delivered",
                    "delivered_content": delivered_content_with_tool_call(
                        "openai/gpt-5-mini", "call_ld", "list_dir", r#"{"path": "."}"#
                    ),
                }
            })))
            .mount(&overpay)
            .await;

        let tmp = TempDir::new().unwrap();
        let s = test_server(seeded_state(&overpay.uri(), &tmp));
        let res = s
            .post("/chat/completions")
            .json(&json!({
                "model": "openai/gpt-5-mini",
                "messages": [{"role": "user", "content": "what's in the cwd?"}],
                "tools": [{"type": "function", "function": {"name": "list_dir", "parameters": {}}}],
                "stream": true,
            }))
            .await;

        res.assert_status_ok();
        let text = res.text();
        let chunks: Vec<Value> = text
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(str::trim)
            .filter(|l| *l != "[DONE]")
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .collect();

        let call_chunk = chunks
            .iter()
            .find(|c| c["choices"][0]["delta"].get("tool_calls").is_some())
            .unwrap_or_else(|| panic!("no tool_calls delta in stream:\n{text}"));
        let call = &call_chunk["choices"][0]["delta"]["tool_calls"][0];
        assert_eq!(call["index"], 0, "streaming fragments group by index");
        assert_eq!(call["id"], "call_ld");
        assert_eq!(call["function"]["name"], "list_dir");

        let finish = chunks
            .iter()
            .filter_map(|c| c["choices"][0]["finish_reason"].as_str())
            .next_back();
        assert_eq!(finish, Some("tool_calls"), "stream:\n{text}");
        assert!(
            text.contains("data: [DONE]"),
            "stream ends with DONE:\n{text}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn buffered_chat_completion_hits_the_iteration_cap_and_errors() {
        let overpay = MockServer::start().await;
        mount_both_listings(&overpay).await;
        mount_order_router(&overpay).await;

        // Every OpenRouter turn calls the tool again -- the conversation
        // never converges to a final answer, and even the forced
        // tool_choice:"none" landing turn yields tool calls with no text
        // (this mock matches OR-10 too), so the cap error still fires.
        Mock::given(method("GET"))
            .and(path_regex(r"^/api/v1/orders/OR-\d+$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "OR-x", "fulfillment_status": "delivered",
                    "delivered_content": delivered_content_with_tool_call(
                        "openai/gpt-5-mini", "call_x", "run_python", r#"{"code": "print(1)"}"#
                    ),
                }
            })))
            .mount(&overpay)
            .await;
        Mock::given(method("GET"))
            .and(path_regex(r"^/api/v1/orders/PY-\d+$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "PY-x", "fulfillment_status": "delivered",
                    "delivered_content": python_delivered_content("1\n", 0),
                }
            })))
            .mount(&overpay)
            .await;

        let tmp = TempDir::new().unwrap();
        let s = test_server(seeded_state(&overpay.uri(), &tmp));

        let res = s
            .post("/chat/completions")
            .json(&json!({
                "model": "openai/gpt-5-mini",
                "messages": [{"role": "user", "content": "loop forever"}],
            }))
            .await;

        res.assert_status(StatusCode::BAD_GATEWAY);
        let body: Value = res.json();
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("safety cap"),
            "body: {body}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn hitting_the_cap_lands_on_a_final_no_tools_turn_instead_of_an_error() {
        // The purchase-loop regression: real orders can be created and paid
        // before the iteration cap trips, so the cap must end in a forced
        // tool_choice:"none" turn that reports what happened — not a 502
        // that throws the context away.
        let overpay = MockServer::start().await;
        mount_both_listings(&overpay).await;
        mount_order_router(&overpay).await;

        // Turns 0-9 (single-digit order ids only): the model keeps
        // checking the same order instead of answering.
        Mock::given(method("GET"))
            .and(path_regex(r"^/api/v1/orders/OR-\d$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "OR-x", "fulfillment_status": "delivered",
                    "delivered_content": delivered_content_with_tool_call(
                        "openai/gpt-5-mini", "call_x", "get_order_status", r#"{"order_id": "ORD-5"}"#
                    ),
                }
            })))
            .mount(&overpay)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/orders/ORD-5"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {"id": "ORD-5", "payment_status": "paid", "fulfillment_status": "delivered"}
            })))
            .mount(&overpay)
            .await;
        // Turn 10 is the forced landing turn: tools disabled, text answer.
        Mock::given(method("GET"))
            .and(path("/api/v1/orders/OR-10"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "OR-10", "fulfillment_status": "delivered",
                    "delivered_content": delivered_content(
                        "Order ORD-5 is paid and delivered.", "openai/gpt-5-mini", false
                    ),
                }
            })))
            .mount(&overpay)
            .await;

        let tmp = TempDir::new().unwrap();
        let s = test_server(seeded_state(&overpay.uri(), &tmp));

        let res = s
            .post("/chat/completions")
            .json(&json!({
                "model": "openai/gpt-5-mini",
                "messages": [{"role": "user", "content": "is my order paid?"}],
            }))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(
            body["choices"][0]["message"]["content"],
            "Order ORD-5 is paid and delivered."
        );
        assert_eq!(body["id"], "chatcmpl-OR-10");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn buffered_chat_completion_recovers_from_an_unknown_tool_name() {
        // A hallucinated tool call must not abort the request -- it feeds
        // back as a tool-result error the model itself can react to, the
        // same way a real coding agent would surface a failed tool call.
        let overpay = MockServer::start().await;
        mount_both_listings(&overpay).await;
        mount_order_router(&overpay).await;

        Mock::given(method("GET"))
            .and(path("/api/v1/orders/OR-0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "OR-0", "fulfillment_status": "delivered",
                    "delivered_content": delivered_content_with_tool_call(
                        "openai/gpt-5-mini", "call_1", "delete_the_universe", "{}"
                    ),
                }
            })))
            .mount(&overpay)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/orders/OR-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "OR-1", "fulfillment_status": "delivered",
                    "delivered_content": delivered_content(
                        "I can't do that, but here's what I can tell you.", "openai/gpt-5-mini", false
                    ),
                }
            })))
            .mount(&overpay)
            .await;

        let tmp = TempDir::new().unwrap();
        let s = test_server(seeded_state(&overpay.uri(), &tmp));

        let res = s
            .post("/chat/completions")
            .json(&json!({
                "model": "openai/gpt-5-mini",
                "messages": [{"role": "user", "content": "do something"}],
            }))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(
            body["choices"][0]["message"]["content"],
            "I can't do that, but here's what I can tell you."
        );
    }

    /// Turn 2's stream: one partial chunk, then delivered. A separate
    /// struct (not the earlier `GrowingBuffer`) because this test's order
    /// id and text differ and the two shouldn't share mutable state anyway.
    struct SecondTurnStream {
        calls: AtomicUsize,
    }
    impl Respond for SecondTurnStream {
        fn respond(&self, _req: &Request) -> ResponseTemplate {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let body = if n == 0 {
                json!({"data": {
                    "id": "OR-1", "fulfillment_status": "processing",
                    "partial_content": "The answer", "partial_seq": 1,
                }})
            } else {
                json!({"data": {
                    "id": "OR-1", "fulfillment_status": "delivered",
                    "delivered_content": delivered_content("The answer is 4.", "openai/gpt-5-mini", false),
                }})
            };
            ResponseTemplate::new(200).set_body_json(body)
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn streaming_executes_a_tool_call_then_streams_the_final_turn() {
        let overpay = MockServer::start().await;
        mount_both_listings(&overpay).await;
        mount_order_router(&overpay).await;

        // Turn 1: a pure tool call -- no text of its own to stream.
        Mock::given(method("GET"))
            .and(path("/api/v1/orders/OR-0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "OR-0", "fulfillment_status": "delivered",
                    "delivered_content": delivered_content_with_tool_call(
                        "openai/gpt-5-mini", "call_1", "run_python", r#"{"code": "print(2+2)"}"#
                    ),
                }
            })))
            .mount(&overpay)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/orders/PY-0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "PY-0", "fulfillment_status": "delivered",
                    "delivered_content": python_delivered_content("4\n", 0),
                }
            })))
            .mount(&overpay)
            .await;
        // Turn 2: streams for real -- proves `streamed` reset to 0 for the
        // new order rather than continuing turn 1's (empty) counter.
        Mock::given(method("GET"))
            .and(path("/api/v1/orders/OR-1"))
            .respond_with(SecondTurnStream {
                calls: AtomicUsize::new(0),
            })
            .mount(&overpay)
            .await;

        let tmp = TempDir::new().unwrap();
        let s = test_server(seeded_state(&overpay.uri(), &tmp));

        let res = s
            .post("/chat/completions")
            .json(&json!({
                "model": "openai/gpt-5-mini",
                "messages": [{"role": "user", "content": "what is 2+2?"}],
                "stream": true,
            }))
            .await;

        res.assert_status_ok();
        let text = res.text();
        let deltas: Vec<String> = text
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(str::trim)
            .filter(|l| *l != "[DONE]")
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter_map(|v| {
                v["choices"][0]["delta"]["content"]
                    .as_str()
                    .map(str::to_string)
            })
            .collect();

        assert_eq!(
            deltas,
            vec!["The answer".to_string(), " is 4.".to_string()],
            "turn 1 (pure tool call) streams nothing; turn 2 streams its partial \
             then catches up the tail:\n{text}"
        );
        assert!(text.trim_end().ends_with("data: [DONE]"), "stream: {text}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn streaming_emits_an_initial_role_delta_and_keeps_one_id_for_the_whole_response() {
        let overpay = MockServer::start().await;
        mount_both_listings(&overpay).await;
        mount_order_router(&overpay).await;

        // Turn 1: a tool call -- proves the id assigned on turn 1's order
        // (OR-0) survives into turn 2 (OR-1), which is a *different* real
        // order. Real OpenAI clients treat `id` as the response's stable
        // identity across every chunk they receive.
        Mock::given(method("GET"))
            .and(path("/api/v1/orders/OR-0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "OR-0", "fulfillment_status": "delivered",
                    "delivered_content": delivered_content_with_tool_call(
                        "openai/gpt-5-mini", "call_1", "run_python", r#"{"code": "print(1)"}"#
                    ),
                }
            })))
            .mount(&overpay)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/orders/PY-0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "PY-0", "fulfillment_status": "delivered",
                    "delivered_content": python_delivered_content("1\n", 0),
                }
            })))
            .mount(&overpay)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/orders/OR-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "OR-1", "fulfillment_status": "delivered",
                    "delivered_content": delivered_content("It's 1.", "openai/gpt-5-mini", false),
                }
            })))
            .mount(&overpay)
            .await;

        let tmp = TempDir::new().unwrap();
        let s = test_server(seeded_state(&overpay.uri(), &tmp));

        let res = s
            .post("/chat/completions")
            .json(&json!({
                "model": "openai/gpt-5-mini",
                "messages": [{"role": "user", "content": "print 1"}],
                "stream": true,
            }))
            .await;

        res.assert_status_ok();
        let text = res.text();
        let chunks: Vec<Value> = text
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(str::trim)
            .filter(|l| *l != "[DONE]")
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .collect();

        assert_eq!(
            chunks[0]["choices"][0]["delta"],
            json!({"role": "assistant"}),
            "the very first chunk must be a bare role delta: {text}"
        );
        let ids: Vec<&str> = chunks.iter().map(|c| c["id"].as_str().unwrap()).collect();
        assert!(
            ids.iter().all(|id| *id == "chatcmpl-OR-0"),
            "every chunk across both turns must share turn 1's id: {ids:?}\n{text}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn streaming_reports_a_stuck_order_as_a_normal_chunk_sequence_not_an_out_of_band_event() {
        // A client that only understands `chat.completion.chunk`-shaped
        // `data:` frames must still see the failure as visible text ending
        // in a real finish_reason + [DONE] -- not a custom `event: error`
        // frame it has no reason to specifically handle.
        let overpay = MockServer::start().await;
        mount_both_listings(&overpay).await;
        mount_fully_paid(&overpay, "O8").await;
        Mock::given(method("GET"))
            .and(path("/api/v1/orders/O8"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {"id": "O8", "fulfillment_status": "awaiting_seller"}
            })))
            .mount(&overpay)
            .await;

        let tmp = TempDir::new().unwrap();
        let s = fast_test_server(seeded_state(&overpay.uri(), &tmp));

        let res = s
            .post("/chat/completions")
            .json(&json!({
                "model": "openai/gpt-5-mini",
                "messages": [{"role": "user", "content": "hi"}],
                "stream": true,
            }))
            .await;

        // The status/headers are already committed to the SSE stream by
        // the time a mid-stream failure is discovered, so this is still
        // 200 -- the failure has to be encoded in the body instead.
        res.assert_status_ok();
        let text = res.text();
        assert!(
            !text.contains("event:"),
            "no custom SSE event framing, only ordinary data: chunks: {text}"
        );

        let chunks: Vec<Value> = text
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(str::trim)
            .filter(|l| *l != "[DONE]")
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .collect();

        let error_text = chunks
            .iter()
            .filter_map(|c| c["choices"][0]["delta"]["content"].as_str())
            .collect::<String>();
        assert!(
            error_text.contains("O8") && error_text.contains("did not complete"),
            "the timeout message must reach the client as visible content: {text}"
        );
        assert_eq!(
            chunks.last().unwrap()["choices"][0]["finish_reason"],
            "stop",
            "must still end with a real finish_reason: {text}"
        );
        assert!(text.trim_end().ends_with("data: [DONE]"), "stream: {text}");
    }

    /// The Python tool order's own poll sequence: one partial chunk of live
    /// stdout, then delivered -- mirrors `SecondTurnStream` but for the
    /// tool-execution leg rather than an OpenRouter turn.
    struct PythonToolStream {
        calls: AtomicUsize,
    }
    impl Respond for PythonToolStream {
        fn respond(&self, _req: &Request) -> ResponseTemplate {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let body = if n == 0 {
                json!({"data": {
                    "id": "PY-0", "fulfillment_status": "processing",
                    "partial_content": "computing", "partial_seq": 1,
                }})
            } else {
                json!({"data": {
                    "id": "PY-0", "fulfillment_status": "delivered",
                    "delivered_content": python_delivered_content("computing... 4\n", 0),
                }})
            };
            ResponseTemplate::new(200).set_body_json(body)
        }
    }

    /// Sequenced forecast order: first poll in flight with a streamed
    /// preview, second poll a delivered report — the streamed prefix plus
    /// the rest, so the client-visible preview and the deliverable agree.
    struct ForecastToolStream {
        calls: AtomicUsize,
    }
    impl Respond for ForecastToolStream {
        fn respond(&self, _req: &Request) -> ResponseTemplate {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let body = if n == 0 {
                json!({"data": {
                    "id": "F-0", "fulfillment_status": "awaiting_seller",
                    "partial_content": "*Consulting the Elder Meteorologists about Reykjavik…*\n",
                    "partial_seq": 1,
                }})
            } else {
                json!({"data": {
                    "id": "F-0", "fulfillment_status": "delivered",
                    "delivered_content": "{\"description\":\"Reykjavik shivers.\",\"image_url\":\"http://localhost:3001/blob.png\"}",
                    "delivered_content_type": "application/json",
                }})
            };
            ResponseTemplate::new(200).set_body_json(body)
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn streaming_forwards_a_listing_tools_preview_unfenced() {
        let overpay = MockServer::start().await;
        mount_both_listings(&overpay).await;
        mount_order_router(&overpay).await;
        mount_listing_tool_catalog(&overpay).await;
        mount_redeem_fully_paid(&overpay, "weather").await;

        Mock::given(method("GET"))
            .and(path("/api/v1/orders/OR-0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "OR-0", "fulfillment_status": "delivered",
                    "delivered_content": delivered_content_with_tool_call(
                        "openai/gpt-5-mini", "call_1", "forecast", r#"{"input": "Reykjavik"}"#
                    ),
                }
            })))
            .mount(&overpay)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/orders/F-0"))
            .respond_with(ForecastToolStream {
                calls: AtomicUsize::new(0),
            })
            .mount(&overpay)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/orders/OR-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "OR-1", "fulfillment_status": "delivered",
                    "delivered_content": delivered_content("Reykjavik shivers.", "openai/gpt-5-mini", false),
                }
            })))
            .mount(&overpay)
            .await;

        let tmp = TempDir::new().unwrap();
        let s = test_server(seeded_state(&overpay.uri(), &tmp));

        let res = s
            .post("/chat/completions")
            .json(&json!({
                "model": "openai/gpt-5-mini",
                "messages": [{"role": "user", "content": "forecast for Reykjavik"}],
                "stream": true,
            }))
            .await;

        res.assert_status_ok();
        let text = res.text();
        let content: String = text
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(str::trim)
            .filter(|l| *l != "[DONE]")
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter_map(|v| {
                v["choices"][0]["delta"]["content"]
                    .as_str()
                    .map(str::to_string)
            })
            .collect();

        assert!(
            content.contains("\n\n*Consulting the Elder Meteorologists about Reykjavik…*\n\n\n"),
            "the preview must reach the client unfenced, set off by blank lines: {content:?}\n{text}"
        );
        assert!(
            !content.contains("```"),
            "listing-tool previews are buyer-facing markdown, not fenced output: {content:?}"
        );
        assert!(
            content.ends_with("Reykjavik shivers."),
            "the final turn's answer must still follow: {content:?}\n{text}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn streaming_forwards_live_python_stdout_wrapped_in_a_code_fence() {
        let overpay = MockServer::start().await;
        mount_both_listings(&overpay).await;
        mount_order_router(&overpay).await;

        Mock::given(method("GET"))
            .and(path("/api/v1/orders/OR-0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "OR-0", "fulfillment_status": "delivered",
                    "delivered_content": delivered_content_with_tool_call(
                        "openai/gpt-5-mini", "call_1", "run_python", r#"{"code": "print(2+2)"}"#
                    ),
                }
            })))
            .mount(&overpay)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/orders/PY-0"))
            .respond_with(PythonToolStream {
                calls: AtomicUsize::new(0),
            })
            .mount(&overpay)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/orders/OR-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "OR-1", "fulfillment_status": "delivered",
                    "delivered_content": delivered_content("The answer is 4.", "openai/gpt-5-mini", false),
                }
            })))
            .mount(&overpay)
            .await;

        let tmp = TempDir::new().unwrap();
        let s = test_server(seeded_state(&overpay.uri(), &tmp));

        let res = s
            .post("/chat/completions")
            .json(&json!({
                "model": "openai/gpt-5-mini",
                "messages": [{"role": "user", "content": "what is 2+2?"}],
                "stream": true,
            }))
            .await;

        res.assert_status_ok();
        let text = res.text();
        let content: String = text
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(str::trim)
            .filter(|l| *l != "[DONE]")
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter_map(|v| {
                v["choices"][0]["delta"]["content"]
                    .as_str()
                    .map(str::to_string)
            })
            .collect();

        assert!(
            content.contains("\n```\ncomputing\n```\n"),
            "live tool stdout must reach the client fenced as it streams: {content:?}\n{text}"
        );
        assert!(
            content.ends_with("The answer is 4."),
            "the final OpenRouter turn's answer must still follow: {content:?}\n{text}"
        );
    }

    // ---- wallet tools: scope gating, projections, spend cap ----

    #[test]
    fn wallet_tool_defs_gate_spending_tools_on_scope() {
        let names = |can_spend: bool| -> Vec<String> {
            wallet_tool_defs(can_spend)
                .iter()
                .map(|d| d["function"]["name"].as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(
            names(false),
            vec![
                GET_BALANCES_TOOL,
                BROWSE_MARKETPLACE_TOOL,
                GET_LISTING_TOOL,
                LIST_ORDERS_TOOL,
                GET_ORDER_STATUS_TOOL
            ]
        );
        assert_eq!(
            names(true),
            vec![
                GET_BALANCES_TOOL,
                BROWSE_MARKETPLACE_TOOL,
                GET_LISTING_TOOL,
                LIST_ORDERS_TOOL,
                GET_ORDER_STATUS_TOOL,
                CREATE_ORDER_TOOL,
                PAY_ORDER_TOOL,
                BUY_CREDITS_TOOL
            ]
        );
    }

    #[test]
    fn projections_never_leak_on_chain_data() {
        // Representative raw payloads deliberately stuffed with every
        // on-chain field the underlying MCP handlers actually return.
        let ledger = SpendLedger::new(20.0);

        let account = json!({
            "address": "0xdeadbeef00000000000000000000000000000000",
            "pubkey": "02abcdef",
            "npub": "npub1secret",
            "zcash_address": "u1qqqsecret",
            "eth_balance": {"raw": 5, "formatted": "0.005", "symbol": "ETH"},
            "usdc_balance": {"raw": 12000000, "formatted": "12.0", "symbol": "USDC"},
            "zec_balance": {"zec": "0.25", "total_zat": 25000000, "spendable_zat": 25000000},
            "account": {"account_number": "1234567890123456"},
            "merchant_credits": {"data": [{
                "id": 7, "holder_type": "seller", "seller_slug": "acme",
                "balance_cents": 500, "formatted_balance": "$5.00",
                "updated_at": "2026-01-01"
            }]},
        });
        let projected = project_balances(&account, &ledger, None).to_string();
        for leak in [
            "0xdeadbeef",
            "02abcdef",
            "npub1secret",
            "u1qqqsecret",
            "1234567890123456",
        ] {
            assert!(!projected.contains(leak), "leaked {leak}: {projected}");
        }
        assert!(projected.contains("12.0"), "balances survive: {projected}");
        {
            // Volatile results carry the moment they were read, so a model
            // resending history can't mistake an old snapshot for current.
            let tmp = TempDir::new().unwrap();
            let state = seeded_state("http://unused.test", &tmp);
            let stamped = stamp_as_of(&state, project_balances(&account, &ledger, None));
            let as_of = stamped.get("as_of").and_then(Value::as_str).unwrap_or("");
            assert!(
                as_of.ends_with("UTC"),
                "as_of stamped in the wallet zone (UTC default): {stamped}"
            );
        }
        assert!(
            projected.contains("acme"),
            "credit holders survive: {projected}"
        );
        assert!(
            projected.contains("remaining_usd"),
            "allowance shown: {projected}"
        );

        let order = json!({"data": {
            "id": "ORD-1", "payment_status": "paid", "fulfillment_status": "delivered",
            "total_usd": "$1.00", "settled_amount_cents": 1,
            "settlement_tx_hash": "0xfeedface",
            "order_url": "https://overpay.example/orders/ORD-1",
        }});
        let projected = project_order_status(&order).to_string();
        assert!(!projected.contains("0xfeedface"), "{projected}");
        assert!(!projected.contains("order_url"), "{projected}");
        assert!(projected.contains("ORD-1") && projected.contains("paid"));
        assert!(
            projected.contains("settled_amount_cents"),
            "the settled charge survives: {projected}"
        );

        let orders = json!({
            "data": [{
                "id": "ORD-9", "product_title": "Widget", "payment_status": "pending",
                "fulfillment_status": "pending", "total_usd": "$2.00",
                "settled_amount_cents": 200,
                "created_at": "2026-08-11T00:00:00Z",
                "settlement_tx_hash": "0xcafebabe",
                "order_url": "https://overpay.example/orders/ORD-9",
                "tracking_number": "1Z999", "buyer_note": "{\"secret\":\"payload\"}",
            }],
            "next_cursor": "abc123",
        });
        let projected = project_orders_list(&orders).to_string();
        for leak in ["0xcafebabe", "order_url", "1Z999", "secret"] {
            assert!(!projected.contains(leak), "leaked {leak}: {projected}");
        }
        assert!(
            projected.contains("ORD-9")
                && projected.contains("Widget")
                && projected.contains("abc123"),
            "rows and cursor survive: {projected}"
        );

        let marketplace = json!({
            "data": [{
                "id": "L-9", "title": "Widget", "description": "A widget",
                "price_usd": "$2.00", "free": false, "category": "tools",
                "seller": {"name": "Acme", "slug": "acme"},
                "main_image_url": "https://cdn.example/widget.png",
                "checkout_url": "http://localhost:4030/checkout/L-9",
                "delivery_eta_seconds": 30,
            }],
            "next_cursor": "cur1",
        });
        let projected = project_marketplace(&marketplace).to_string();
        for leak in ["main_image_url", "checkout_url", "cdn.example"] {
            assert!(!projected.contains(leak), "leaked {leak}: {projected}");
        }
        assert!(
            projected.contains("L-9") && projected.contains("acme") && projected.contains("cur1"),
            "rows and cursor survive: {projected}"
        );

        let listing_detail = json!({"data": {
            "id": "L-9", "title": "Widget", "description": "The full description",
            "price_usd": "$2.00",
            "seller": {"name": "Acme", "slug": "acme"},
            "checkout_url": "http://localhost:4030/checkout/L-9",
            "buyer_note_schema": {"type": "object", "properties": {"color": {"type": "string"}}},
            "checkout_schema": {"type": "object"},
        }});
        let projected = project_listing_detail(&listing_detail).to_string();
        assert!(
            !projected.contains("checkout"),
            "checkout URL/schema stay out: {projected}"
        );
        assert!(
            projected.contains("buyer_note_schema") && projected.contains("full description"),
            "ordering-flow fields survive: {projected}"
        );

        let buy = json!({
            "order_id": "ORD-2", "tx_hash": "0xbeef", "payment_amount_usdc": 5.0,
            "order_url": "https://overpay.example/orders/ORD-2",
            "status": "payment_sent", "note": "Credits will be funded automatically once the transfer is detected on-chain.",
        });
        let projected = project_buy(&buy, 5.0, &ledger, None).to_string();
        assert!(!projected.contains("0xbeef") && !projected.contains("order_url"));
        assert!(projected.contains("ORD-2") && projected.contains("payment_sent"));
    }

    #[test]
    fn order_status_projection_returns_the_deliverable_capped() {
        // Inline deliverable under the cap passes through whole.
        let small = json!({"data": {
            "id": "O1", "payment_status": "paid", "fulfillment_status": "delivered",
            "delivered_content": "{\"description\":\"Sunny, 22C\",\"image_url\":\"https://img.example/w.png\"}",
            "delivered_content_type": "application/json",
        }});
        let p = project_order_status(&small);
        assert!(p["delivered_content"].as_str().unwrap().contains("Sunny"));
        assert_eq!(p["delivered_content_type"], "application/json");
        assert!(p.get("delivered_content_truncated").is_none());

        // Oversized content is cut at the cap and flagged.
        let big = json!({"data": {
            "id": "O2",
            "delivered_content": "y".repeat(DELIVERED_CONTENT_MODEL_CAP + 500),
        }});
        let p = project_order_status(&big);
        assert_eq!(
            p["delivered_content"].as_str().unwrap().len(),
            DELIVERED_CONTENT_MODEL_CAP
        );
        assert_eq!(p["delivered_content_truncated"], json!(true));

        // A download URL wins over any lingering inline blob.
        let with_url = json!({"data": {
            "id": "O3",
            "delivered_content_url": "https://overpay.example/blob/abc",
            "delivered_content": "stale inline copy",
        }});
        let p = project_order_status(&with_url);
        assert_eq!(
            p["delivered_content_url"],
            "https://overpay.example/blob/abc"
        );
        assert!(p.get("delivered_content").is_none());
    }

    #[test]
    fn spend_ledger_reserves_releases_and_records() {
        let mut ledger = SpendLedger::new(10.0);
        assert!(ledger.try_spend(4.0).is_ok());
        assert!(
            ledger.try_spend(7.0).is_err(),
            "4 + 7 exceeds the 10 USD cap"
        );
        ledger.release(4.0);
        assert!(ledger.try_spend(7.0).is_ok());
        ledger.record(3.0);
        assert_eq!(ledger.remaining_usd(), 0.0);
        assert!(ledger.try_spend(0.01).is_err());
        assert!(ledger.try_spend(f64::NAN).is_err());
        assert!(ledger.try_spend(-1.0).is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spend_scoped_key_executes_pay_order_and_confirms_via_redeem() {
        let overpay = MockServer::start().await;
        mount_both_listings(&overpay).await;
        mount_order_router(&overpay).await;

        // Turn 1: the model pays a pending order by id.
        Mock::given(method("GET"))
            .and(path("/api/v1/orders/OR-0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "OR-0", "fulfillment_status": "delivered",
                    "delivered_content": delivered_content_with_tool_call(
                        "openai/gpt-5-mini", "call_1", "pay_order", r#"{"order_id": "ORD-77"}"#
                    ),
                }
            })))
            .mount(&overpay)
            .await;
        // The order pay_order resolves the seller from.
        Mock::given(method("GET"))
            .and(path("/api/v1/orders/ORD-77"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "ORD-77", "payment_status": "pending",
                    "fulfillment_status": "pending",
                    "listing": {"id": "L-ACME", "title": "Widget"},
                }
            })))
            .mount(&overpay)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/listings/L-ACME"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {"id": "L-ACME", "title": "Widget", "seller": {"name": "Acme", "slug": "acme"}}
            })))
            .mount(&overpay)
            .await;
        // The actual settlement — exactly one redemption proves the tool ran.
        Mock::given(method("POST"))
            .and(path("/api/v1/merchant_credits/acme/redeem"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {"status": "fully_paid", "amount_redeemed_cents": 100,
                         "credit_balance_cents": 400}
            })))
            .expect(1)
            .mount(&overpay)
            .await;
        // Turn 2: with the tool result in hand, the model answers.
        Mock::given(method("GET"))
            .and(path("/api/v1/orders/OR-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "OR-1", "fulfillment_status": "delivered",
                    "delivered_content": delivered_content("Paid — order ORD-77 is settled.", "openai/gpt-5-mini", false),
                }
            })))
            .mount(&overpay)
            .await;

        let tmp = TempDir::new().unwrap();
        let s = test_server_with_scopes(seeded_state(&overpay.uri(), &tmp), "chat spend");

        let res = s
            .post("/chat/completions")
            .json(&json!({
                "model": "openai/gpt-5-mini",
                "messages": [{"role": "user", "content": "pay order ORD-77"}],
            }))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(
            body["choices"][0]["message"]["content"],
            "Paid — order ORD-77 is settled."
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn chat_scoped_key_cannot_execute_a_spending_tool_the_model_hallucinates() {
        let overpay = MockServer::start().await;
        mount_both_listings(&overpay).await;
        mount_order_router(&overpay).await;

        // The model calls pay_order even though a chat-scoped request never
        // advertised it. Zero marketplace mocks for the payment side: any
        // attempt to actually execute would 404 the mock server; instead the
        // refusal feeds back and the model recovers.
        Mock::given(method("GET"))
            .and(path("/api/v1/orders/OR-0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "OR-0", "fulfillment_status": "delivered",
                    "delivered_content": delivered_content_with_tool_call(
                        "openai/gpt-5-mini", "call_1", "pay_order", r#"{"order_id": "ORD-1"}"#
                    ),
                }
            })))
            .mount(&overpay)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/orders/OR-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "OR-1", "fulfillment_status": "delivered",
                    "delivered_content": delivered_content(
                        "I don't have spending permission on this key.", "openai/gpt-5-mini", false
                    ),
                }
            })))
            .mount(&overpay)
            .await;

        let tmp = TempDir::new().unwrap();
        let s = test_server(seeded_state(&overpay.uri(), &tmp));

        let res = s
            .post("/chat/completions")
            .json(&json!({
                "model": "openai/gpt-5-mini",
                "messages": [{"role": "user", "content": "pay order ORD-1"}],
            }))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(
            body["choices"][0]["message"]["content"],
            "I don't have spending permission on this key."
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn buy_credits_beyond_the_spend_cap_is_refused_without_touching_the_wallet() {
        let overpay = MockServer::start().await;
        mount_both_listings(&overpay).await;
        mount_order_router(&overpay).await;

        // A purchase attempt would hit this — expect(0) proves the cap
        // refused the spend before anything reached the marketplace.
        Mock::given(method("POST"))
            .and(path("/api/v1/merchant_credits/acme/purchase"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "order_id": "never", "order_url": "never"
            })))
            .expect(0)
            .mount(&overpay)
            .await;

        Mock::given(method("GET"))
            .and(path("/api/v1/orders/OR-0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "OR-0", "fulfillment_status": "delivered",
                    "delivered_content": delivered_content_with_tool_call(
                        "openai/gpt-5-mini", "call_1", "buy_credits",
                        r#"{"seller_slug": "acme", "amount_usd": 999999.0}"#
                    ),
                }
            })))
            .mount(&overpay)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/orders/OR-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "OR-1", "fulfillment_status": "delivered",
                    "delivered_content": delivered_content(
                        "That exceeds this request's spending allowance.", "openai/gpt-5-mini", false
                    ),
                }
            })))
            .mount(&overpay)
            .await;

        let tmp = TempDir::new().unwrap();
        let s = test_server_with_scopes(seeded_state(&overpay.uri(), &tmp), "chat spend");

        let res = s
            .post("/chat/completions")
            .json(&json!({
                "model": "openai/gpt-5-mini",
                "messages": [{"role": "user", "content": "load a million dollars"}],
            }))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(
            body["choices"][0]["message"]["content"],
            "That exceeds this request's spending allowance."
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dashboard_set_spend_cap_overrides_the_default_per_request() {
        let overpay = MockServer::start().await;
        mount_both_listings(&overpay).await;
        mount_order_router(&overpay).await;

        // $10 fits the built-in $20 cap but not the wallet's stored $5
        // override — expect(0) proves the stored cap refused the spend.
        Mock::given(method("POST"))
            .and(path("/api/v1/merchant_credits/acme/purchase"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "order_id": "never", "order_url": "never"
            })))
            .expect(0)
            .mount(&overpay)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/orders/OR-0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "OR-0", "fulfillment_status": "delivered",
                    "delivered_content": delivered_content_with_tool_call(
                        "openai/gpt-5-mini", "call_1", "buy_credits",
                        r#"{"seller_slug": "acme", "amount_usd": 10.0}"#
                    ),
                }
            })))
            .mount(&overpay)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/orders/OR-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "OR-1", "fulfillment_status": "delivered",
                    "delivered_content": delivered_content(
                        "That exceeds this request's spending allowance.", "openai/gpt-5-mini", false
                    ),
                }
            })))
            .mount(&overpay)
            .await;

        let tmp = TempDir::new().unwrap();
        let state = seeded_state(&overpay.uri(), &tmp);
        // The wallet-level cap is read per request — set after the router
        // exists, no restart involved.
        state
            .db
            .lock()
            .unwrap()
            .write_spend_cap_usd_cents(Some(500))
            .unwrap();
        let s = test_server_with_scopes(state, "chat spend");

        let res = s
            .post("/chat/completions")
            .json(&json!({
                "model": "openai/gpt-5-mini",
                "messages": [{"role": "user", "content": "load ten dollars"}],
            }))
            .await;
        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(
            body["choices"][0]["message"]["content"],
            "That exceeds this request's spending allowance."
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn buy_credits_beyond_the_key_budget_is_refused_without_touching_the_wallet() {
        let overpay = MockServer::start().await;
        mount_both_listings(&overpay).await;
        mount_order_router(&overpay).await;

        // $10 fits the $20 per-request cap but not this key's $5 daily
        // budget — expect(0) proves the budget refused the spend before
        // anything reached the marketplace.
        Mock::given(method("POST"))
            .and(path("/api/v1/merchant_credits/acme/purchase"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "order_id": "never", "order_url": "never"
            })))
            .expect(0)
            .mount(&overpay)
            .await;

        Mock::given(method("GET"))
            .and(path("/api/v1/orders/OR-0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "OR-0", "fulfillment_status": "delivered",
                    "delivered_content": delivered_content_with_tool_call(
                        "openai/gpt-5-mini", "call_1", "buy_credits",
                        r#"{"seller_slug": "acme", "amount_usd": 10.0}"#
                    ),
                }
            })))
            .mount(&overpay)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/orders/OR-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "OR-1", "fulfillment_status": "delivered",
                    "delivered_content": delivered_content(
                        "That exceeds this key's budget.", "openai/gpt-5-mini", false
                    ),
                }
            })))
            .mount(&overpay)
            .await;

        let tmp = TempDir::new().unwrap();
        let state = seeded_state(&overpay.uri(), &tmp);
        let s = test_server_with_key(state.clone(), "chat spend", Some(500));

        let res = s
            .post("/chat/completions")
            .json(&json!({
                "model": "openai/gpt-5-mini",
                "messages": [{"role": "user", "content": "load ten dollars of credits"}],
            }))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(
            body["choices"][0]["message"]["content"],
            "That exceeds this key's budget."
        );
        // The refused reservation must not eat the budget — only the two
        // chat turns' own operating cost (2¢ each, per the redeem mock)
        // was recorded.
        let keys = state
            .db
            .lock()
            .unwrap()
            .list_provider_keys("npub1abandon")
            .unwrap();
        assert_eq!(keys[0].spent_today_usd_cents(), 4);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn key_budget_persists_across_requests_and_exhausts() {
        let overpay = MockServer::start().await;
        mount_both_listings(&overpay).await;
        mount_order_router(&overpay).await;

        // Budget $1.05. Request 1: turn OR-0 (2¢ operating cost) +
        // pay_order ($1.00 redemption) + turn OR-1 (2¢) = $1.04 — 1¢
        // remains, so request 2 passes the up-front gate. Its first turn
        // OR-2 (2¢) overshoots; the pay_order call then refuses on the
        // exhausted budget, and the loop breaks to the landing turn OR-3.
        // expect(1) on the acme redeem endpoint proves only the first
        // settlement happened (chat orders redeem under openrouter-bot).
        for turn in ["OR-0", "OR-2"] {
            Mock::given(method("GET"))
                .and(path(format!("/api/v1/orders/{turn}")))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "data": {
                        "id": turn, "fulfillment_status": "delivered",
                        "delivered_content": delivered_content_with_tool_call(
                            "openai/gpt-5-mini", "call_1", "pay_order",
                            r#"{"order_id": "ORD-77", "seller_slug": "acme"}"#
                        ),
                    }
                })))
                .mount(&overpay)
                .await;
        }
        for (turn, text) in [("OR-1", "Paid."), ("OR-3", "This key's budget is spent.")] {
            Mock::given(method("GET"))
                .and(path(format!("/api/v1/orders/{turn}")))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "data": {
                        "id": turn, "fulfillment_status": "delivered",
                        "delivered_content": delivered_content(text, "openai/gpt-5-mini", false),
                    }
                })))
                .mount(&overpay)
                .await;
        }
        // pay_order always fetches the order first (already-paid check).
        Mock::given(method("GET"))
            .and(path("/api/v1/orders/ORD-77"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "ORD-77", "payment_status": "pending",
                    "fulfillment_status": "pending",
                    "listing": {"id": "L-ACME", "title": "Widget"},
                }
            })))
            .mount(&overpay)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/merchant_credits/acme/redeem"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {"status": "fully_paid", "amount_redeemed_cents": 100,
                         "credit_balance_cents": 400}
            })))
            .expect(1)
            .mount(&overpay)
            .await;

        let tmp = TempDir::new().unwrap();
        let state = seeded_state(&overpay.uri(), &tmp);
        let s = test_server_with_key(state.clone(), "chat spend", Some(105));

        for expected in ["Paid.", "This key's budget is spent."] {
            let res = s
                .post("/chat/completions")
                .json(&json!({
                    "model": "openai/gpt-5-mini",
                    "messages": [{"role": "user", "content": "pay order ORD-77"}],
                }))
                .await;
            res.assert_status_ok();
            let body: Value = res.json();
            assert_eq!(body["choices"][0]["message"]["content"], expected);
        }

        let keys = state
            .db
            .lock()
            .unwrap()
            .list_provider_keys("npub1abandon")
            .unwrap();
        assert_eq!(
            keys[0].spent_today_usd_cents(),
            108,
            "the settlement plus four 2¢ chat turns were recorded"
        );
        assert_eq!(keys[0].remaining_today_usd_cents(), Some(0));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn chat_turn_operating_cost_counts_against_the_key_budget() {
        let overpay = MockServer::start().await;
        mount_both_listings(&overpay).await;
        mount_order_router(&overpay).await;
        Mock::given(method("GET"))
            .and(path("/api/v1/orders/OR-0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "OR-0", "fulfillment_status": "delivered",
                    "delivered_content": delivered_content("Hi.", "openai/gpt-5-mini", false),
                }
            })))
            .mount(&overpay)
            .await;

        let tmp = TempDir::new().unwrap();
        let state = seeded_state(&overpay.uri(), &tmp);
        // A chat-only key: no spending tools, but chat turns still cost.
        let s = test_server_with_key(state.clone(), "chat", Some(500));

        let res = s
            .post("/chat/completions")
            .json(&json!({
                "model": "openai/gpt-5-mini",
                "messages": [{"role": "user", "content": "hello"}],
            }))
            .await;
        res.assert_status_ok();

        let keys = state
            .db
            .lock()
            .unwrap()
            .list_provider_keys("npub1abandon")
            .unwrap();
        assert_eq!(
            keys[0].spent_today_usd_cents(),
            2,
            "the turn's own redemption (2¢ mock) counts against the budget"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn metered_delivery_nets_the_settlement_refund_out_of_the_key_budget() {
        let overpay = MockServer::start().await;
        mount_both_listings(&overpay).await;
        mount_order_router(&overpay).await;
        // A metered turn: pay time records the gross 2¢ deposit, but the
        // seller settled the order down to 1¢ and states that final charge
        // in the delivered payload — the other 1¢ goes back to the budget.
        Mock::given(method("GET"))
            .and(path("/api/v1/orders/OR-0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "OR-0", "fulfillment_status": "delivered",
                    "delivered_content": serde_json::to_string(&json!({
                        "description": "Hi.", "model": "openai/gpt-5-mini",
                        "error": false, "credits_refunded": false,
                        "usage": {"prompt_tokens": 10, "completion_tokens": 5, "cost": 0.008},
                        "charged_cents": 1,
                    }))
                    .unwrap(),
                }
            })))
            .mount(&overpay)
            .await;

        let tmp = TempDir::new().unwrap();
        let state = seeded_state(&overpay.uri(), &tmp);
        let s = test_server_with_key(state.clone(), "chat", Some(500));

        let res = s
            .post("/chat/completions")
            .json(&json!({
                "model": "openai/gpt-5-mini",
                "messages": [{"role": "user", "content": "hello"}],
            }))
            .await;
        res.assert_status_ok();

        // The reported cost is the *settled* charge, not the gross deposit,
        // and it agrees exactly with what the key budget recorded — the two
        // read the same delivery, so they can never tell the user different
        // stories about what a turn cost.
        let body: Value = res.json();
        assert_eq!(body["usage"]["charged_cents"], json!(1), "body: {body}");
        assert_eq!(body["usage"]["cost"], json!(0.01), "body: {body}");
        assert_eq!(body["usage"]["prompt_tokens"], json!(10), "body: {body}");
        assert_eq!(body["usage"]["completion_tokens"], json!(5), "body: {body}");
        assert_eq!(body["usage"]["total_tokens"], json!(15), "body: {body}");

        let keys = state
            .db
            .lock()
            .unwrap()
            .list_provider_keys("npub1abandon")
            .unwrap();
        assert_eq!(
            body["usage"]["charged_cents"].as_i64(),
            Some(keys[0].spent_today_usd_cents()),
            "reported cost must match the budget's own accounting"
        );
        assert_eq!(
            keys[0].spent_today_usd_cents(),
            1,
            "gross 2¢ at pay time, 1¢ handed back once the delivery stated its final charge"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn exhausted_daily_budget_refuses_a_new_request_up_front() {
        let overpay = MockServer::start().await;
        mount_both_listings(&overpay).await;
        mount_order_router(&overpay).await;

        let tmp = TempDir::new().unwrap();
        let state = seeded_state(&overpay.uri(), &tmp);
        let s = test_server_with_key(state.clone(), "chat", Some(100));
        {
            let db = state.db.lock().unwrap();
            let keys = db.list_provider_keys("npub1abandon").unwrap();
            db.record_provider_key_spend(&keys[0].id, 100).unwrap();
        }

        let res = s
            .post("/chat/completions")
            .json(&json!({
                "model": "openai/gpt-5-mini",
                "messages": [{"role": "user", "content": "hello"}],
            }))
            .await;
        res.assert_status(axum::http::StatusCode::PAYMENT_REQUIRED);
        let body: Value = res.json();
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("daily budget exhausted"),
            "got: {body}"
        );
    }

    #[test]
    fn listing_cost_note_reads_the_listing_price_fields() {
        assert_eq!(
            listing_cost_note(&json!({"price_usd": "$1.50", "free": false})),
            " Each call places a real marketplace order billed to the wallet (≈ $1.50 per call)."
        );
        // A free listing still places an order — say so without a price.
        assert_eq!(
            listing_cost_note(&json!({"price_usd": "Free", "free": true})),
            " Each call places a real marketplace order (this listing is currently free)."
        );
        // `formatted_price`'s "Free" sentinel alone is enough.
        assert_eq!(
            listing_cost_note(&json!({"price_usd": "Free"})),
            " Each call places a real marketplace order (this listing is currently free)."
        );
        // No price fields at all (older serializations): generic but honest.
        assert_eq!(
            listing_cost_note(&json!({})),
            " Each call places a real marketplace order billed to the wallet."
        );
    }

    #[test]
    fn tool_names_are_validated_conservatively() {
        assert!(valid_tool_name("forecast"));
        assert!(valid_tool_name("run_javascript"));
        assert!(valid_tool_name("a-b_C9"));
        assert!(!valid_tool_name(""));
        assert!(!valid_tool_name("has space"));
        assert!(!valid_tool_name("emoji✨"));
        assert!(!valid_tool_name(&"x".repeat(65)));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn listing_tool_registry_filters_wraps_and_skips_collisions() {
        let overpay = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/listings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [
                    // Bare-string schema → wrapped parameters.
                    forecast_index_row(),
                    // Object schema → passed through verbatim.
                    {"id": "L-OBJ", "title": "Objective", "description": "obj",
                     "seller": {"slug": "obj"},
                     "provider_tool": {"name": "objective"}},
                    // Colliding with the hardcoded run_python: skipped.
                    {"id": "L-PY2", "title": "Sneaky Python", "seller": {"slug": "sneak"},
                     "provider_tool": {"name": "run_python"}},
                    // Colliding with a wallet tool: skipped.
                    {"id": "L-BAL", "title": "Sneaky Balances", "seller": {"slug": "sneak"},
                     "provider_tool": {"name": "get_balances"}},
                    // Invalid name: skipped.
                    {"id": "L-BAD", "title": "Bad", "seller": {"slug": "bad"},
                     "provider_tool": {"name": "has space"}},
                    // Duplicate of forecast: first wins.
                    {"id": "L-DUP", "title": "Dup", "seller": {"slug": "dup"},
                     "provider_tool": {"name": "forecast"}},
                    // Unmarked listing: ignored.
                    {"id": "L-PLAIN", "title": "Plain", "seller": {"slug": "plain"}},
                ]
            })))
            .mount(&overpay)
            .await;
        // Only the survivors get a detail fetch — the skipped candidates
        // are filtered before any per-listing GET (no mocks for them, so
        // an eager fetch would 404 and fail the test).
        Mock::given(method("GET"))
            .and(path(format!("/api/v1/listings/{FORECAST_ID}")))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"data": forecast_show_body()})),
            )
            .mount(&overpay)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/listings/L-OBJ"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {"id": "L-OBJ", "title": "Objective", "description": "obj",
                         "buyer_note_schema": {"type": "object", "properties": {"q": {"type": "string"}}},
                         "seller": {"slug": "obj"},
                         "provider_tool": {"name": "objective"}}
            })))
            .mount(&overpay)
            .await;

        let tmp = TempDir::new().unwrap();
        let state = seeded_state(&overpay.uri(), &tmp);
        let tools = match fetch_listing_tools(&state).await {
            Ok(t) => t,
            Err(e) => panic!("fetch failed: {}", e.message()),
        };

        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["forecast", "objective"]);

        let forecast = &tools[0];
        assert_eq!(forecast.listing_id, FORECAST_ID);
        assert_eq!(forecast.seller_slug, "weather");
        // The roster itself must mark each listing tool as a paid order,
        // priced from the listing's own fields (fathom-x/norm#17).
        assert!(
            forecast.description.ends_with(
                "Each call places a real marketplace order billed to the wallet \
                 (≈ $0.10 per call)."
            ),
            "cost note missing: {}",
            forecast.description
        );
        assert!(forecast.wrapped, "bare-string schema must be wrapped");
        assert_eq!(forecast.parameters["type"], "object");
        assert_eq!(forecast.parameters["properties"]["input"]["type"], "string");
        assert_eq!(forecast.parameters["required"][0], "input");

        let objective = &tools[1];
        assert!(!objective.wrapped);
        assert_eq!(objective.parameters["properties"]["q"]["type"], "string");

        // Buyer-note unwrapping matches the wrapping.
        assert_eq!(
            listing_tool_buyer_note(forecast, &json!({"input": "Galveston"})),
            json!("Galveston")
        );
        assert_eq!(
            listing_tool_buyer_note(objective, &json!({"q": "hi"})),
            json!({"q": "hi"})
        );
    }

    #[test]
    fn listing_delivery_projection_handles_failure_and_caps_content() {
        let failed = extract_listing_delivered(
            "F-9",
            &json!({"data": {"fulfillment_status": "failed", "fulfillment_error": "no gpu",
                             "settlement_tx_hash": "0xdeadbeef"}}),
        );
        assert_eq!(failed["order_id"], "F-9");
        assert_eq!(failed["error"], "no gpu");
        assert!(
            !failed.to_string().contains("0xdeadbeef"),
            "projection must stay chain-free: {failed}"
        );

        let big = "x".repeat(DELIVERED_CONTENT_MODEL_CAP + 100);
        let capped = extract_listing_delivered(
            "F-10",
            &json!({"data": {"fulfillment_status": "delivered", "delivered_content": big,
                             "delivered_content_type": "text/plain"}}),
        );
        assert_eq!(
            capped["delivered_content"].as_str().unwrap().len(),
            DELIVERED_CONTENT_MODEL_CAP
        );
        assert_eq!(capped["delivered_content_truncated"], true);
        assert_eq!(capped["delivered_content_type"], "text/plain");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn marked_listing_is_advertised_and_executed_as_a_tool() {
        let overpay = MockServer::start().await;
        mount_both_listings(&overpay).await;
        let note_slot = mount_order_router_capturing(&overpay).await;
        mount_listing_tool_catalog(&overpay).await;
        mount_redeem_fully_paid(&overpay, "weather").await;

        // Turn 1: the model calls the forecast listing tool.
        Mock::given(method("GET"))
            .and(path("/api/v1/orders/OR-0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "OR-0", "fulfillment_status": "delivered",
                    "delivered_content": delivered_content_with_tool_call(
                        "openai/gpt-5-mini", "call_1", "forecast", r#"{"input": "Galveston"}"#
                    ),
                }
            })))
            .mount(&overpay)
            .await;
        // The forecast order itself: a real, separately-paid order that
        // delivers the report JSON.
        Mock::given(method("GET"))
            .and(path("/api/v1/orders/F-0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "F-0", "fulfillment_status": "delivered",
                    "delivered_content": "{\"description\":\"Galveston steams.\",\"image_url\":\"http://localhost:3001/rails/active_storage/blobs/redirect/real/delivered.png\"}",
                    "delivered_content_type": "application/json",
                }
            })))
            .mount(&overpay)
            .await;
        // Turn 2: with the real delivered report in hand, the model answers.
        Mock::given(method("GET"))
            .and(path("/api/v1/orders/OR-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "OR-1", "fulfillment_status": "delivered",
                    "delivered_content": delivered_content("Galveston steams.", "openai/gpt-5-mini", false),
                }
            })))
            .mount(&overpay)
            .await;

        let tmp = TempDir::new().unwrap();
        let s = test_server(seeded_state(&overpay.uri(), &tmp));

        let res = s
            .post("/chat/completions")
            .json(&json!({
                "model": "openai/gpt-5-mini",
                "messages": [{"role": "user", "content": "order a weather report for Galveston"}],
            }))
            .await;
        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(
            body["choices"][0]["message"]["content"],
            "Galveston steams."
        );

        // The buyer_note reached the marketplace as the raw string —
        // unwrapped from {input: ...} and NOT JSON-quoted, so the bot's
        // `buyer_note.to_s` sees clean text.
        assert_eq!(
            *note_slot.lock().unwrap(),
            Some(json!("Galveston")),
            "buyer_note must be the verbatim location string"
        );
    }

    // ---- one-shot marketplace tools on the MCP surface ----
    // These exercise crate::tools' dynamic roster, which reuses this
    // module's purchase helpers — hence they live beside its mocks.

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mcp_marketplace_specs_advertise_run_python_and_marked_listings() {
        let overpay = MockServer::start().await;
        mount_both_listings(&overpay).await;
        mount_listing_tool_catalog(&overpay).await;

        let tmp = TempDir::new().unwrap();
        let state = seeded_state(&overpay.uri(), &tmp);
        let specs = crate::tools::marketplace_specs(&state).await;
        let names: Vec<&str> = specs
            .iter()
            .filter_map(|s| s.get("name").and_then(Value::as_str))
            .collect();
        assert!(names.contains(&"run_python"), "specs: {names:?}");
        assert!(names.contains(&"forecast"), "specs: {names:?}");
        for spec in &specs {
            assert!(
                spec.get("inputSchema").is_some_and(Value::is_object),
                "every spec carries an object schema: {spec}"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mcp_run_python_is_a_one_shot_sanitized_purchase() {
        let overpay = MockServer::start().await;
        mount_both_listings(&overpay).await;
        mount_order_router(&overpay).await;
        // Delivered content stuffed with a field outside the documented
        // shape — the projection must strip it.
        Mock::given(method("GET"))
            .and(path("/api/v1/orders/PY-0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "PY-0", "fulfillment_status": "delivered",
                    "delivered_content":
                        "{\"stdout\":\"2\\n\",\"stderr\":\"\",\"exit_code\":0,\"txid\":\"0xdeadbeef\"}",
                }
            })))
            .mount(&overpay)
            .await;

        let tmp = TempDir::new().unwrap();
        let state = seeded_state(&overpay.uri(), &tmp);
        let out = crate::tools::dispatch_sanitized(
            &state,
            "run_python",
            json!({"code": "print(1+1)"}),
            None,
        )
        .await
        .expect("one-shot run_python");
        assert_eq!(out.data["stdout"], "2\n");
        assert_eq!(out.data["exit_code"], 0);
        assert!(
            out.data.get("txid").is_none(),
            "undocumented delivered fields must not survive sanitization: {}",
            out.data
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mcp_marked_listing_tool_is_a_one_shot_purchase() {
        let overpay = MockServer::start().await;
        mount_both_listings(&overpay).await;
        let note_slot = mount_order_router_capturing(&overpay).await;
        mount_listing_tool_catalog(&overpay).await;
        mount_redeem_fully_paid(&overpay, "weather").await;
        Mock::given(method("GET"))
            .and(path("/api/v1/orders/F-0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "F-0", "fulfillment_status": "delivered",
                    "delivered_content": "{\"description\":\"Galveston steams.\"}",
                    "delivered_content_type": "application/json",
                }
            })))
            .mount(&overpay)
            .await;

        let tmp = TempDir::new().unwrap();
        let state = seeded_state(&overpay.uri(), &tmp);
        let out = crate::tools::dispatch_sanitized(
            &state,
            "forecast",
            json!({"input": "Galveston"}),
            None,
        )
        .await
        .expect("one-shot forecast");
        assert_eq!(out.data["order_id"], "F-0");
        assert!(
            out.data["delivered_content"]
                .as_str()
                .unwrap()
                .contains("Galveston steams."),
            "deliverable survives sanitization: {}",
            out.data
        );
        // Wrapped bare-schema unwrap still applies on this surface.
        assert_eq!(*note_slot.lock().unwrap(), Some(json!("Galveston")));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mcp_provider_key_session_carries_v1_money_rules() {
        let overpay = MockServer::start().await;
        let tmp = TempDir::new().unwrap();
        let state = seeded_state(&overpay.uri(), &tmp);
        let (chat_key, _) = state
            .db
            .lock()
            .unwrap()
            .create_provider_key("npub1abandon", "chat-only", "chat", None)
            .unwrap();
        let (spend_key, _) = state
            .db
            .lock()
            .unwrap()
            .create_provider_key("npub1abandon", "spender", "chat spend", None)
            .unwrap();

        // Chat-scoped key: spending tools refuse on scope, before any
        // marketplace traffic (no mocks mounted — a network call would 404
        // into a different error).
        let chat_state = state.with_provider_key(chat_key.id.clone(), false);
        for tool in ["create_order", "pay_order", "buy", "load_core_credits"] {
            let err = crate::tools::dispatch(&chat_state, tool, json!({}), None)
                .await
                .expect_err("chat-scoped key must not spend");
            assert!(err.to_string().contains("chat-scoped"), "{tool}: {err}");
        }

        // Raw-address sends refuse for ANY provider key, spend scope
        // included — they belong to the wallet owner's own hands.
        let spend_state = state.with_provider_key(spend_key.id.clone(), true);
        for tool in ["send_usdc", "send_zcash"] {
            let err = crate::tools::dispatch(&spend_state, tool, json!({}), None)
                .await
                .expect_err("provider keys must not reach raw sends");
            assert!(err.to_string().contains("dashboard"), "{tool}: {err}");
        }

        // Sessions without a provider key (OAuth / local) are untouched:
        // the same call proceeds past the gate (and fails later on the
        // unmocked marketplace instead — proving the gate didn't fire).
        let err = crate::tools::dispatch(&state, "pay_order", json!({"order_id": "X"}), None)
            .await
            .expect_err("unmocked marketplace");
        assert!(
            !err.to_string().contains("chat-scoped"),
            "no scope gate without a key: {err}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mcp_one_shot_purchase_records_against_the_key_budget() {
        let overpay = MockServer::start().await;
        mount_both_listings(&overpay).await;
        mount_order_router(&overpay).await;
        Mock::given(method("GET"))
            .and(path("/api/v1/orders/PY-0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "PY-0", "fulfillment_status": "delivered",
                    "delivered_content": python_delivered_content("2\n", 0),
                }
            })))
            .mount(&overpay)
            .await;

        let tmp = TempDir::new().unwrap();
        let state = seeded_state(&overpay.uri(), &tmp);
        // Chat-scoped: one-shots are operating cost, allowed like /v1's
        // own turns — but recorded.
        let (key, _) = state
            .db
            .lock()
            .unwrap()
            .create_provider_key("npub1abandon", "chat-only", "chat", Some(500))
            .unwrap();
        let key_state = state.with_provider_key(key.id.clone(), false);

        crate::tools::dispatch_sanitized(&key_state, "run_python", json!({"code": "1"}), None)
            .await
            .expect("one-shot allowed for a chat key");
        let keys = state
            .db
            .lock()
            .unwrap()
            .list_provider_keys("npub1abandon")
            .unwrap();
        let row = keys.iter().find(|k| k.id == key.id).unwrap();
        assert_eq!(
            row.spent_today_usd_cents(),
            2,
            "the 2¢ redeem must land on the key's budget"
        );

        // An exhausted budget refuses the next one-shot up front.
        let (broke_key, _) = state
            .db
            .lock()
            .unwrap()
            .create_provider_key("npub1abandon", "broke", "chat", Some(0))
            .unwrap();
        let broke_state = state.with_provider_key(broke_key.id, false);
        let err = crate::tools::dispatch(&broke_state, "run_python", json!({"code": "1"}), None)
            .await
            .expect_err("exhausted budget refuses");
        assert!(err.to_string().contains("budget"), "{err}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mcp_pay_order_records_the_redemption_on_the_key() {
        let overpay = MockServer::start().await;
        mount_redeem_fully_paid(&overpay, "acme").await;
        Mock::given(method("GET"))
            .and(path("/api/v1/orders/ORD-77"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {"id": "ORD-77", "payment_status": "pending"}
            })))
            .mount(&overpay)
            .await;

        let tmp = TempDir::new().unwrap();
        let state = seeded_state(&overpay.uri(), &tmp);
        let (key, _) = state
            .db
            .lock()
            .unwrap()
            .create_provider_key("npub1abandon", "spender", "chat spend", Some(500))
            .unwrap();
        let key_state = state.with_provider_key(key.id.clone(), true);

        let out = crate::tools::dispatch_sanitized(
            &key_state,
            "pay_order",
            json!({"order_id": "ORD-77", "seller_slug": "acme"}),
            None,
        )
        .await
        .expect("spend key pays");
        assert_eq!(out.data["status"], "fully_paid", "{}", out.data);

        let keys = state
            .db
            .lock()
            .unwrap()
            .list_provider_keys("npub1abandon")
            .unwrap();
        let row = keys.iter().find(|k| k.id == key.id).unwrap();
        assert_eq!(
            row.spent_today_usd_cents(),
            2,
            "the redemption records against the key, same as /v1"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mcp_unknown_tool_name_still_errors() {
        let overpay = MockServer::start().await;
        mount_both_listings(&overpay).await;
        mount_listing_tool_catalog(&overpay).await;

        let tmp = TempDir::new().unwrap();
        let state = seeded_state(&overpay.uri(), &tmp);
        let err = crate::tools::dispatch(&state, "no_such_tool", json!({}), None)
            .await
            .expect_err("unknown names still reject");
        assert!(err.to_string().contains("unknown tool"), "got: {err}");
    }

    #[test]
    fn balances_projection_reports_the_key_budget() {
        let ledger = SpendLedger::new(20.0);
        let key = owallet_db::ProviderKeyRow {
            id: "k1".into(),
            npub: "npub1abandon".into(),
            created_at: 0,
            label: None,
            token_prefix: None,
            scopes: Some("chat spend".into()),
            daily_budget_usd_cents: Some(2500),
            spent_usd_cents: 1000,
            spent_day: None,
        };
        let projected = project_balances(&json!({}), &ledger, Some(&key));
        assert_eq!(projected["key_budget"]["daily_budget_usd"], json!(25.0));
        assert_eq!(projected["key_budget"]["spent_today_usd"], json!(10.0));
        assert_eq!(projected["key_budget"]["remaining_today_usd"], json!(15.0));
        // Identity fields never ride along.
        let text = projected.to_string();
        assert!(!text.contains("npub1abandon") && !text.contains("\"k1\""));

        // No-limit key: budget/remaining are null, spend still visible.
        let unlimited = owallet_db::ProviderKeyRow {
            daily_budget_usd_cents: None,
            ..key
        };
        let projected = project_balances(&json!({}), &ledger, Some(&unlimited));
        assert_eq!(projected["key_budget"]["daily_budget_usd"], Value::Null);
        assert_eq!(projected["key_budget"]["remaining_today_usd"], Value::Null);
        assert_eq!(projected["key_budget"]["spent_today_usd"], json!(10.0));

        // Chat-only requests carry no key handle → no key_budget at all.
        let projected = project_balances(&json!({}), &ledger, None);
        assert!(projected.get("key_budget").is_none());
    }
}
