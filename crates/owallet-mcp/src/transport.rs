//! Streamable-HTTP MCP transport.
//!
//! One `POST /mcp` endpoint accepts JSON-RPC 2.0 requests. The handler
//! dispatches `initialize`, `tools/list`, `tools/call`, and `ping`. All
//! other requests get `Method not found`.
//!
//! Authorization is optional: callers can mount the router with
//! [`mcp_router`] (no auth) or with [`mcp_router_with_auth`] (a closure
//! that extracts the `Authorization` header and returns the wallet npub
//! the call should run as).

use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::{Json, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use serde_json::{json, Value};

use crate::jsonrpc::{codes, ErrorObject, Request, Response as RpcResponse};
use crate::progress::ProgressSink;
use crate::state::McpState;
use crate::tools::{self, ToolError};

/// Function the host application provides to map the `Authorization`
/// header to a wallet npub. The handler doesn't care *how* the mapping
/// is done — the caller (typically `owallet-http`) can look the bearer
/// up in the local OAuth AS's access-token table.
pub type BearerAuthCheck = Arc<dyn Fn(Option<&str>) -> AuthResult + Send + Sync + 'static>;

#[derive(Debug, Clone)]
pub enum AuthResult {
    /// Request is anonymous (only public tools allowed).
    Anonymous,
    /// Request is bound to this wallet npub.
    Wallet(String),
    /// Token was supplied but is invalid / expired / unknown.
    Invalid,
}

#[derive(Clone)]
struct RouterState {
    mcp: McpState,
    auth: Option<BearerAuthCheck>,
}

/// Build a `Router` mounted at `/mcp` with no auth (every request runs as
/// anonymous-or-default-wallet). Useful for local-only deployments.
pub fn mcp_router(state: McpState) -> Router {
    Router::new()
        .route("/", post(handle))
        .with_state(RouterState {
            mcp: state,
            auth: None,
        })
}

/// Same as [`mcp_router`] but every request goes through `auth` to map the
/// bearer token to a wallet npub.
pub fn mcp_router_with_auth(state: McpState, auth: BearerAuthCheck) -> Router {
    Router::new()
        .route("/", post(handle))
        .with_state(RouterState {
            mcp: state,
            auth: Some(auth),
        })
}

async fn handle(
    State(state): State<RouterState>,
    headers: HeaderMap,
    Json(req): Json<Request>,
) -> Response {
    // Resolve npub from bearer if an auth callback was supplied.
    let mcp_state = if let Some(auth) = &state.auth {
        let bearer = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer "))
            .map(|s| s.trim());
        match auth(bearer) {
            AuthResult::Invalid => {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(RpcResponse::err(
                        req.id.unwrap_or(Value::Null),
                        codes::INVALID_REQUEST,
                        "invalid or expired bearer token",
                    )),
                )
                    .into_response();
            }
            AuthResult::Anonymous => state.mcp.with_npub(None),
            AuthResult::Wallet(n) => state.mcp.with_npub(Some(n)),
        }
    } else {
        state.mcp.clone()
    };

    let id_for_response = req.id.clone().unwrap_or(Value::Null);
    let is_notification = req.id.is_none();

    // Streamable-HTTP: when the client accepts an SSE stream, answer a
    // `tools/call` over one so the tool can push `notifications/progress`
    // frames ahead of its final result. Every other method — and any
    // client that only accepts JSON — falls through to the single-body
    // path below, so this is purely additive and backward-compatible.
    if !is_notification && req.method == "tools/call" && accepts_event_stream(&headers) {
        if let Some(params) = req.params.clone() {
            return sse_tools_call(mcp_state, id_for_response, params);
        }
    }

    let resp = match req.method.as_str() {
        "initialize" => initialize_result(),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools::catalog() })),
        "tools/call" => match req.params {
            Some(p) => tools_call(&mcp_state, p).await,
            None => Err(JrpcError {
                code: codes::INVALID_PARAMS,
                message: "tools/call requires params".into(),
            }),
        },
        _ => Err(JrpcError {
            code: codes::METHOD_NOT_FOUND,
            message: format!("unknown method '{}'", req.method),
        }),
    };

    if is_notification {
        // Per JSON-RPC 2.0, notifications get no response body. We still
        // return 204 so the caller knows the request was accepted.
        return StatusCode::NO_CONTENT.into_response();
    }

    let body = match resp {
        Ok(result) => RpcResponse::ok(id_for_response, result),
        Err(e) => RpcResponse::err(id_for_response, e.code, e.message),
    };
    Json(body).into_response()
}

fn initialize_result() -> Result<Value, JrpcError> {
    Ok(json!({
        "protocolVersion": "2024-11-05",
        "serverInfo": {
            "name": "owallet",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "capabilities": {
            "tools": { "listChanged": false }
        },
    }))
}

/// Parsed `tools/call` params. `_meta.progressToken` is the client's
/// opt-in signal for streamed progress (ignored on the buffered path).
#[derive(serde::Deserialize)]
struct ToolCallParams {
    name: String,
    #[serde(default)]
    arguments: Value,
    #[serde(default, rename = "_meta")]
    meta: Option<CallMeta>,
}

#[derive(serde::Deserialize)]
struct CallMeta {
    #[serde(default, rename = "progressToken")]
    progress_token: Option<Value>,
}

async fn tools_call(state: &McpState, params: Value) -> Result<Value, JrpcError> {
    let params: ToolCallParams = serde_json::from_value(params).map_err(|e| JrpcError {
        code: codes::INVALID_PARAMS,
        message: format!("bad tools/call params: {e}"),
    })?;

    let outcome = tools::dispatch_sanitized(state, &params.name, params.arguments, None).await;
    Ok(tool_result_value(outcome))
}

/// Wrap a tool outcome as the MCP `tools/call` result value:
/// `{ content: [...], structuredContent, isError }`. The model only sees
/// `content`, so the rendered, model-facing summary (`out.text`, built by
/// `crate::render`) goes there while the structured payload sits in
/// `structuredContent` for programmatic clients (fathom-x/overpay#295).
/// Both legs arrive pre-sanitized of on-chain data by
/// `tools::dispatch_sanitized` (fathom-x/overpay#391).
/// Errors render to a friendly, actionable message via `render_error`.
/// Shared by the buffered and streamed paths so both frame results
/// identically.
fn tool_result_value(outcome: Result<tools::ToolOutput, ToolError>) -> Value {
    match outcome {
        Ok(out) => json!({
            "content": [{
                "type": "text",
                "text": out.text,
            }],
            "structuredContent": out.data,
            "isError": false,
        }),
        Err(e) => json!({
            "content": [{
                "type": "text",
                "text": crate::render::render_error(&e),
            }],
            "isError": true,
        }),
    }
}

/// Does the client accept an SSE stream? (`Accept: text/event-stream`,
/// possibly alongside `application/json`.)
fn accepts_event_stream(headers: &HeaderMap) -> bool {
    headers
        .get(axum::http::header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(|accept| {
            accept
                .split(',')
                .any(|part| part.trim().starts_with("text/event-stream"))
        })
        .unwrap_or(false)
}

/// One SSE event carrying a single JSON-RPC message in its `data` field,
/// per the MCP Streamable-HTTP transport. Serializing these small,
/// controlled structs is infallible in practice; fall back to `{}` rather
/// than panicking on the astronomically unlikely error.
fn sse_data(value: &impl serde::Serialize) -> Event {
    let body = serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string());
    Event::default().data(body)
}

/// Answer a `tools/call` over Server-Sent Events: forward every
/// `notifications/progress` the tool emits, then close the stream with the
/// tool's final JSON-RPC response as the last event. The tool runs inline
/// in the stream (no spawn), so it borrows the moved-in `state` and sink
/// for the duration.
fn sse_tools_call(state: McpState, id: Value, params: Value) -> Response {
    let stream = async_stream::stream! {
        let ToolCallParams { name, arguments, meta } = match serde_json::from_value(params) {
            Ok(p) => p,
            Err(e) => {
                let resp = RpcResponse::err(
                    id.clone(),
                    codes::INVALID_PARAMS,
                    format!("bad tools/call params: {e}"),
                );
                yield Ok::<_, Infallible>(sse_data(&resp));
                return;
            }
        };

        let token = meta.and_then(|m| m.progress_token);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
        let sink = ProgressSink::new(tx, token);

        // Drive the tool and the progress channel concurrently: yield each
        // progress notification as it arrives, break out with the result
        // once the tool completes.
        let fut = tools::dispatch_sanitized(&state, &name, arguments, Some(&sink));
        tokio::pin!(fut);
        let outcome = loop {
            tokio::select! {
                biased;
                Some(note) = rx.recv() => {
                    yield Ok(sse_data(&note));
                }
                out = &mut fut => break out,
            }
        };

        // Flush anything emitted in the same tick the tool returned.
        while let Ok(note) = rx.try_recv() {
            yield Ok(sse_data(&note));
        }

        let resp = RpcResponse::ok(id, tool_result_value(outcome));
        yield Ok(sse_data(&resp));
    };

    Sse::new(stream).into_response()
}

struct JrpcError {
    code: i32,
    message: String,
}

// Keep `ErrorObject` re-exported so callers building raw envelopes can use it.
#[allow(dead_code)]
fn _ensure_error_object_exported() -> ErrorObject {
    ErrorObject {
        code: 0,
        message: String::new(),
        data: None,
    }
}

// Keep ToolError tied to the public surface so unused-imports doesn't
// strip the import.
#[allow(dead_code)]
fn _ensure_tool_error(_e: ToolError) {}
