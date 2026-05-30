//! JSON-RPC 2.0 message types used by the MCP transport.
//!
//! Spec: <https://www.jsonrpc.org/specification>. The MCP protocol layers
//! a few well-known methods on top — `initialize`, `tools/list`,
//! `tools/call`, `ping` — which are parsed by `transport.rs` and dispatched
//! to the tool registry in `tools.rs`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Deserialize)]
pub struct Request {
    #[serde(default = "default_version")]
    pub jsonrpc: String,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
    /// Request id — `null`, number, or string. Notifications (no id) are
    /// allowed by the spec but MCP doesn't use them, so we treat missing
    /// id as a notification and skip the response.
    #[serde(default)]
    pub id: Option<Value>,
}

fn default_version() -> String {
    "2.0".to_string()
}

#[derive(Debug, Clone, Serialize)]
pub struct Response {
    pub jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorObject>,
    pub id: Value,
}

impl Response {
    pub fn ok(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            result: Some(result),
            error: None,
            id,
        }
    }

    pub fn err(id: Value, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            result: None,
            error: Some(ErrorObject {
                code,
                message: message.into(),
                data: None,
            }),
            id,
        }
    }

    pub fn err_with_data(id: Value, code: i32, message: impl Into<String>, data: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            result: None,
            error: Some(ErrorObject {
                code,
                message: message.into(),
                data: Some(data),
            }),
            id,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorObject {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// Standard JSON-RPC error codes.
pub mod codes {
    pub const PARSE_ERROR: i32 = -32700;
    pub const INVALID_REQUEST: i32 = -32600;
    pub const METHOD_NOT_FOUND: i32 = -32601;
    pub const INVALID_PARAMS: i32 = -32602;
    pub const INTERNAL_ERROR: i32 = -32603;
}
