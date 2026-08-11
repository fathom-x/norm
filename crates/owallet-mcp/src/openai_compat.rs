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
//! advertises exactly one tool to the model — `run_python`, backed by the
//! "Run Python Code" listing (`code_executor` bot, seller slug `exec`).
//! When the model decides to call it, this module executes it by placing
//! and paying for a *second, real* Overpay order, feeds stdout/stderr back
//! to the model, and loops until a turn produces no more tool calls. The
//! HTTP caller never sees a `tool_call` — just a normal chat completion
//! that happens to have run code along the way. Each iteration is a real,
//! separately-paid order (see [`MAX_TOOL_ITERATIONS`]); caller-supplied
//! `tools` are not accepted — this endpoint owns tool selection, since it
//! is the one actually executing them.
//!
//! Requests require a wallet-scoped provider API key issued by the dashboard.
//! It binds spending to that wallet; [`McpState::resolve_owned_auth`] then
//! authenticates it to Overpay for every order.

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
use tokio::sync::OnceCell;

use crate::state::{McpState, OwnedAuth, ResolveAuthError};
use crate::tools::{new_output_since, partial_output, WAIT_TERMINAL_STATUSES};
use owallet_overpay::models::ListingFilters;
use owallet_overpay::OverpayError;

const OPENROUTER_SELLER_SLUG: &str = "openrouter-bot";
const OPENROUTER_LISTING_TITLE: &str = "OpenRouter Inference";
const PYTHON_SELLER_SLUG: &str = "exec";
const PYTHON_LISTING_TITLE: &str = "Run Python Code";
const RUN_PYTHON_TOOL_NAME: &str = "run_python";

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

/// Hard cap on OpenRouter turns per chat completion request. Each
/// iteration that ends in a tool call is a *real, separately-paid* order
/// against the Python listing on top of the OpenRouter order itself — a
/// model that keeps calling tools (retry loops, "let me check that again")
/// would otherwise spend the wallet's credits without bound. Reached means
/// the request fails rather than spending further; it does not mean
/// something is broken.
const MAX_TOOL_ITERATIONS: u32 = 4;

/// How long a single order (an OpenRouter turn, or a run_python execution)
/// waits to finish before giving up. OpenAI's own API has no
/// server-advertised timeout (that's a client-side concern), but
/// *something* has to bound how long we hold the HTTP connection open for
/// a stuck order.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
/// Poll cadence, matching `wait_for_order`'s own floor/default — see that
/// tool's docs for why 1s: the marketplace's own broadcast fan-out
/// (Solid Cable) polls at roughly the same granularity today.
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Resolved listing ids, process-wide. Each id is derived from its bot's
/// private key (`Overpay::Uuid.derive_listing_id`) and so differs per
/// environment — neither can be a literal constant — but it never changes
/// for a given Overpay instance during a process's lifetime, so a plain
/// cache (no invalidation) is enough.
static OPENROUTER_LISTING_ID: OnceCell<String> = OnceCell::const_new();
static PYTHON_LISTING_ID: OnceCell<String> = OnceCell::const_new();

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
    let ctx = Ctx {
        mcp: state,
        timeout,
        poll,
    };
    Router::new()
        .route("/models", get(list_models))
        .route("/chat/completions", post(chat_completions))
        .with_state(ctx)
}

// ---------------------------------------------------------------------------
// Errors — OpenAI's `{error: {message, type, param, code}}` envelope, so an
// unmodified OpenAI-compatible client's error handling still works.
// ---------------------------------------------------------------------------

enum OpenAiError {
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

    fn message(&self) -> &str {
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
    let mcp = authenticate_provider_key(&ctx.mcp, &headers)?;
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

async fn resolve_openrouter_listing_id(state: &McpState) -> Result<String, OpenAiError> {
    resolve_listing_id_cached(
        state,
        OPENROUTER_SELLER_SLUG,
        OPENROUTER_LISTING_TITLE,
        &OPENROUTER_LISTING_ID,
    )
    .await
}

async fn resolve_python_listing_id(state: &McpState) -> Result<String, OpenAiError> {
    resolve_listing_id_cached(
        state,
        PYTHON_SELLER_SLUG,
        PYTHON_LISTING_TITLE,
        &PYTHON_LISTING_ID,
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

/// The `run_python` tool definition offered to the model on every turn.
/// `parameters` is the Python listing's own `buyer_note_schema` — already
/// valid JSON Schema shaped exactly like an OpenAI tool's `parameters`
/// field expects (`{code, stdin, requirements, requirements_lock}`), so
/// this can't drift from what the listing actually accepts the way a
/// hand-duplicated schema could.
async fn run_python_tool_def(state: &McpState) -> Result<Value, OpenAiError> {
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
    // Caller-supplied `tools`/`tool_choice` are deliberately not read: this
    // endpoint always offers exactly one tool (`run_python`) and executes
    // it itself — see the module doc. A caller's own tool definitions
    // would produce tool_calls nothing here knows how to run.
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
    let mcp = match authenticate_provider_key(&ctx.mcp, &headers) {
        Ok(mcp) => mcp,
        Err(e) => return e.into_response(),
    };
    let ctx = Ctx { mcp, ..ctx };
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
/// its wallet. Provider key verifiers live in SQLite, so a database copy does
/// not reveal usable spending credentials.
fn authenticate_provider_key(
    state: &McpState,
    headers: &HeaderMap,
) -> Result<McpState, OpenAiError> {
    let value = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|token| !token.is_empty())
        .ok_or_else(|| OpenAiError::Unauthorized("missing provider API key".into()))?;
    let npub = state
        .db
        .lock()
        .map_err(|e| OpenAiError::internal(format!("db mutex: {e}")))?
        .read_provider_key_npub(value)
        .map_err(|e| OpenAiError::internal(format!("provider key lookup: {e}")))?
        .ok_or_else(|| OpenAiError::Unauthorized("invalid provider API key".into()))?;
    Ok(state.with_npub(Some(npub)))
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
) -> Result<String, OpenAiError> {
    let note_str = serde_json::to_string(buyer_note)
        .map_err(|e| OpenAiError::internal(format!("could not encode buyer_note: {e}")))?;

    let order = state
        .overpay
        .create_order_value(listing_id, Some(&note_str), auth.as_auth())
        .await?;
    let order_id = order
        .get("data")
        .and_then(|d| d.get("id"))
        .and_then(Value::as_str)
        .ok_or_else(|| OpenAiError::internal("create_order response missing id"))?
        .to_string();

    let redeem = state
        .overpay
        .redeem_merchant_credits_value(seller_slug, &order_id, auth.as_auth())
        .await?;
    let status = redeem
        .get("data")
        .and_then(|d| d.get("status"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if status != "fully_paid" && status != "already_paid" {
        let message = redeem
            .get("data")
            .and_then(|d| d.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("insufficient Overpay merchant credits");
        return Err(OpenAiError::PaymentRequired(format!(
            "{message} — load more with the wallet's `load_core_credits` MCP tool or the dashboard"
        )));
    }

    Ok(order_id)
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
    loop {
        let snap = state
            .overpay
            .get_order_value(order_id, auth.as_auth())
            .await?;
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

/// What the OpenRouter listing actually delivered, parsed out of the order
/// snapshot's (JSON-string-encoded) `delivered_content`.
struct OpenRouterDelivered {
    text: String,
    model: String,
    error: bool,
    tool_calls: Vec<Value>,
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
    })
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
async fn execute_tool_call(
    state: &McpState,
    auth: &OwnedAuth,
    call: &Value,
    timeout: Duration,
    poll: Duration,
) -> String {
    let name = call
        .pointer("/function/name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if name != RUN_PYTHON_TOOL_NAME {
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

    match run_python_tool(state, auth, &arguments, timeout, poll).await {
        Ok(result) => result.to_string(),
        Err(e) => json!({"error": e.message()}).to_string(),
    }
}

async fn run_python_tool(
    state: &McpState,
    auth: &OwnedAuth,
    arguments: &Value,
    timeout: Duration,
    poll: Duration,
) -> Result<Value, OpenAiError> {
    let listing_id = resolve_python_listing_id(state).await?;
    let order_id =
        place_and_pay_order(state, auth, &listing_id, PYTHON_SELLER_SLUG, arguments).await?;
    let snap = wait_for_order_terminal(state, auth, &order_id, timeout, poll).await?;
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
    content: String,
}

/// One OpenRouter turn's outcome once the agentic loop is done: either a
/// final answer, or (internally, before the loop decides) a tool call to
/// execute and feed back.
struct AgentResult {
    text: String,
    model: String,
    order_id: String,
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
    let tool_def = run_python_tool_def(&ctx.mcp).await?;
    let mut last_model = requested_model.to_string();

    for _ in 0..MAX_TOOL_ITERATIONS {
        let buyer_note = json!({
            "model": requested_model,
            "messages": messages,
            "tools": [tool_def],
            "tool_choice": "auto",
        });
        let order_id = place_and_pay_order(
            &ctx.mcp,
            auth,
            &listing_id,
            OPENROUTER_SELLER_SLUG,
            &buyer_note,
        )
        .await?;
        let snap =
            wait_for_order_terminal(&ctx.mcp, auth, &order_id, ctx.timeout, ctx.poll).await?;
        let delivered = extract_openrouter_delivered(&snap)?;
        if delivered.error {
            return Err(OpenAiError::UpstreamFailure(delivered.text));
        }
        if !delivered.model.is_empty() {
            last_model = delivered.model;
        }

        if delivered.tool_calls.is_empty() {
            return Ok(AgentResult {
                text: delivered.text,
                model: last_model,
                order_id,
            });
        }

        messages.push(json!({
            "role": "assistant",
            "content": if delivered.text.is_empty() { Value::Null } else { json!(delivered.text) },
            "tool_calls": delivered.tool_calls,
        }));
        for call in &delivered.tool_calls {
            let result_text = execute_tool_call(&ctx.mcp, auth, call, ctx.timeout, ctx.poll).await;
            let tool_call_id = call.get("id").and_then(Value::as_str).unwrap_or_default();
            messages.push(
                json!({ "role": "tool", "tool_call_id": tool_call_id, "content": result_text }),
            );
        }
    }

    Err(OpenAiError::UpstreamFailure(format!(
        "the model kept calling tools past the {MAX_TOOL_ITERATIONS}-iteration safety cap \
         (each call is a real, paid order) — stopping rather than spending further"
    )))
}

async fn buffered_chat_completion(
    ctx: &Ctx,
    req: ChatCompletionRequest,
) -> Result<ChatCompletionResponse, OpenAiError> {
    let (_npub, auth) = ctx.mcp.resolve_owned_auth()?;
    let messages = normalize_messages(&req.messages)?;
    let result = run_agentic_loop(ctx, &auth, messages, &req.model).await?;

    Ok(ChatCompletionResponse {
        id: format!("chatcmpl-{}", result.order_id),
        object: "chat.completion",
        created: unix_now(),
        model: result.model,
        choices: vec![ChatCompletionChoice {
            index: 0,
            message: ChatMessageOut {
                role: "assistant",
                content: result.text,
            },
            finish_reason: "stop",
        }],
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

/// The suffix of `delivered_text` not yet covered by `streamed` bytes —
/// catches up a client whose order finished before the first poll ever
/// observed a partial chunk (fast/tiny replies), or whose partial buffer
/// was truncated by the marketplace's 32 KB cap while delivered_content
/// was not. `None` once fully caught up.
fn catch_up(delivered_text: &str, streamed: usize) -> Option<&str> {
    if streamed >= delivered_text.len() || !delivered_text.is_char_boundary(streamed) {
        return None;
    }
    Some(&delivered_text[streamed..])
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
        let python_listing_id = match resolve_python_listing_id(&ctx.mcp).await {
            Ok(id) => id,
            Err(e) => {
                for ev in error_events("error", &requested_model, e) { yield Ok(ev); }
                return;
            }
        };
        let tool_def = match run_python_tool_def(&ctx.mcp).await {
            Ok(d) => d,
            Err(e) => {
                for ev in error_events("error", &requested_model, e) { yield Ok(ev); }
                return;
            }
        };

        let mut last_model = requested_model.clone();
        // Filled in once the first order places; every chunk after that —
        // across every turn and every tool execution — reuses it.
        let mut response_id = String::new();

        for _ in 0..MAX_TOOL_ITERATIONS {
            let buyer_note = json!({
                "model": requested_model,
                "messages": messages,
                "tools": [tool_def.clone()],
                "tool_choice": "auto",
            });
            let order_id = match place_and_pay_order(&ctx.mcp, &auth, &listing_id, OPENROUTER_SELLER_SLUG, &buyer_note).await {
                Ok(id) => id,
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

            let mut streamed = 0usize;
            let start = Instant::now();
            let snap = loop {
                let snap = match ctx.mcp.overpay.get_order_value(&order_id, auth.as_auth()).await {
                    Ok(s) => s,
                    Err(e) => {
                        for ev in error_events(&response_id, &last_model, OpenAiError::from(e)) { yield Ok(ev); }
                        return;
                    }
                };

                let (partial, _seq) = partial_output(&snap);
                match new_output_since(partial, &mut streamed) {
                    Some(delta) => yield Ok(chunk_event(&response_id, &last_model, json!({"content": delta}), None)),
                    None => yield Ok(Event::default().comment("owallet: waiting on the model")),
                }

                if is_terminal(order_status(&snap)) {
                    break snap;
                }
                if start.elapsed() >= ctx.timeout {
                    let err = OpenAiError::UpstreamFailure(format!(
                        "order {order_id} did not complete within {}s", ctx.timeout.as_secs()
                    ));
                    for ev in error_events(&response_id, &last_model, err) { yield Ok(ev); }
                    return;
                }
                tokio::time::sleep(ctx.poll).await;
            };

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

            if let Some(tail) = catch_up(&delivered.text, streamed) {
                yield Ok(chunk_event(&response_id, &last_model, json!({"content": tail}), None));
            }

            if delivered.tool_calls.is_empty() {
                yield Ok(chunk_event(&response_id, &last_model, json!({}), Some("stop")));
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

                if name != RUN_PYTHON_TOOL_NAME {
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

                let python_order_id = match place_and_pay_order(&ctx.mcp, &auth, &python_listing_id, PYTHON_SELLER_SLUG, &arguments).await {
                    Ok(id) => id,
                    Err(e) => {
                        let result_text = json!({"error": e.message()}).to_string();
                        messages.push(json!({ "role": "tool", "tool_call_id": tool_call_id, "content": result_text }));
                        continue 'tool_calls;
                    }
                };

                let mut py_streamed = 0usize;
                let mut fence_open = false;
                let py_start = Instant::now();
                let python_snap;
                loop {
                    let snap = match ctx.mcp.overpay.get_order_value(&python_order_id, auth.as_auth()).await {
                        Ok(s) => s,
                        Err(e) => {
                            let result_text = json!({"error": OpenAiError::from(e).message()}).to_string();
                            messages.push(json!({ "role": "tool", "tool_call_id": tool_call_id, "content": result_text }));
                            continue 'tool_calls;
                        }
                    };

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

                let result_text = match extract_python_delivered(&python_snap) {
                    Ok(result) => result.to_string(),
                    Err(e) => json!({"error": e.message()}).to_string(),
                };
                messages.push(json!({ "role": "tool", "tool_call_id": tool_call_id, "content": result_text }));
            }
        }

        let id = if response_id.is_empty() { "error" } else { response_id.as_str() };
        let err = OpenAiError::UpstreamFailure(format!(
            "the model kept calling tools past the {MAX_TOOL_ITERATIONS}-iteration safety cap \
             (each call is a real, paid order) — stopping rather than spending further"
        ));
        for ev in error_events(id, &last_model, err) { yield Ok(ev); }
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

    /// The exact `run_python` tool definition `run_python_tool_def` builds
    /// from `python_listing_body`'s fixture schema+description — every
    /// OpenRouter turn carries this in `buyer_note.tools`, so a test that
    /// asserts on the exact buyer_note string needs it too.
    fn expected_run_python_tool_def() -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "run_python",
                "description": "Run a Python 3.11 snippet in an isolated sandbox.",
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

    fn python_delivered_content(stdout: &str, exit_code: i64) -> String {
        serde_json::to_string(&json!({
            "stdout": stdout, "stderr": "", "exit_code": exit_code,
            "duration_ms": 42, "timed_out": false
        }))
        .unwrap()
    }

    fn delivered_content(description: &str, model: &str, error: bool) -> String {
        serde_json::to_string(&json!({
            "description": description, "model": model,
            "error": error, "credits_refunded": false
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
        let key = state
            .db
            .lock()
            .unwrap()
            .create_provider_key("npub1abandon", "test")
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
            .create_provider_key("npub1abandon", "test")
            .unwrap()
            .1;
        let app = router_with_timing(state, Duration::from_millis(150), Duration::from_millis(30));
        let mut server = TestServer::new(app).unwrap();
        server.add_header(header::AUTHORIZATION, format!("Bearer {key}"));
        server
    }

    #[test]
    fn provider_key_is_required_and_binds_the_wallet() {
        let tmp = TempDir::new().unwrap();
        let state = seeded_state("http://127.0.0.1:1", &tmp);
        let key = state
            .db
            .lock()
            .unwrap()
            .create_provider_key("npub1abandon", "test")
            .unwrap()
            .1;

        assert!(authenticate_provider_key(&state, &HeaderMap::new()).is_err());

        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {key}").parse().unwrap(),
        );
        let authenticated = match authenticate_provider_key(&state, &headers) {
            Ok(state) => state,
            Err(_) => panic!("valid provider key should authenticate"),
        };
        assert_eq!(authenticated.active_npub.as_deref(), Some("npub1abandon"));
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
            "tools": [ expected_run_python_tool_def() ],
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
            "tools": [ expected_run_python_tool_def() ],
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

        let finish_reasons: Vec<Value> = text
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(str::trim)
            .filter(|l| *l != "[DONE]")
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .map(|v| v["choices"][0]["finish_reason"].clone())
            .collect();
        assert_eq!(
            finish_reasons.last(),
            Some(&Value::String("stop".to_string())),
            "final chunk must carry finish_reason=stop: {text}"
        );
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
    struct OrderCreateRouter {
        openrouter_calls: AtomicUsize,
        python_calls: AtomicUsize,
    }
    impl Respond for OrderCreateRouter {
        fn respond(&self, req: &Request) -> ResponseTemplate {
            let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
            let listing_id = body
                .get("listing_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let id = if listing_id == OPENROUTER_ID {
                format!(
                    "OR-{}",
                    self.openrouter_calls.fetch_add(1, Ordering::SeqCst)
                )
            } else if listing_id == PYTHON_ID {
                format!("PY-{}", self.python_calls.fetch_add(1, Ordering::SeqCst))
            } else {
                panic!("unexpected listing_id in create_order body: {body}");
            };
            ResponseTemplate::new(201)
                .set_body_json(json!({"data": {"id": id, "payment_status": "pending"}}))
        }
    }

    async fn mount_order_router(overpay: &MockServer) {
        Mock::given(method("POST"))
            .and(path("/api/v1/orders"))
            .respond_with(OrderCreateRouter {
                openrouter_calls: AtomicUsize::new(0),
                python_calls: AtomicUsize::new(0),
            })
            .mount(overpay)
            .await;
        mount_redeem_fully_paid(overpay, "openrouter-bot").await;
        mount_redeem_fully_paid(overpay, "exec").await;
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
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn buffered_chat_completion_hits_the_iteration_cap_and_errors() {
        let overpay = MockServer::start().await;
        mount_both_listings(&overpay).await;
        mount_order_router(&overpay).await;

        // Every OpenRouter turn calls the tool again -- the conversation
        // never converges to a final answer.
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
}
