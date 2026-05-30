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

use std::sync::Arc;

use axum::extract::{Json, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use serde_json::{json, Value};

use crate::jsonrpc::{codes, ErrorObject, Request, Response as RpcResponse};
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

async fn tools_call(state: &McpState, params: Value) -> Result<Value, JrpcError> {
    #[derive(serde::Deserialize)]
    struct ToolCallParams {
        name: String,
        #[serde(default)]
        arguments: Value,
    }

    let params: ToolCallParams = serde_json::from_value(params).map_err(|e| JrpcError {
        code: codes::INVALID_PARAMS,
        message: format!("bad tools/call params: {e}"),
    })?;

    let outcome = tools::dispatch(state, &params.name, params.arguments).await;
    // MCP wraps tool output as `{ content: [...], isError: bool }`.
    // `ToolOutput::Json(v)` is the common case: one pretty-text block
    // plus structuredContent. `ToolOutput::Content(blocks)` lets a
    // tool emit multiple content blocks (used by `get_account_info`
    // to match the Python tool's markdown-table + JSON dual output).
    Ok(match outcome {
        Ok(tools::ToolOutput::Json(value)) => json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string_pretty(&value).unwrap_or_default(),
            }],
            "structuredContent": value,
            "isError": false,
        }),
        Ok(tools::ToolOutput::Content(blocks)) => json!({
            "content": blocks
                .into_iter()
                .map(|b| json!({"type": "text", "text": b.text}))
                .collect::<Vec<_>>(),
            "isError": false,
        }),
        Err(e) => json!({
            "content": [{
                "type": "text",
                "text": e.to_string(),
            }],
            "isError": true,
        }),
    })
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
