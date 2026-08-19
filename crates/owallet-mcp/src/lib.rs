//! Streamable-HTTP MCP server for owallet.
//!
//! Ports the tool surface of `wallet_mcp/server.py:1418-2118` as a
//! hand-rolled JSON-RPC 2.0 handler. The protocol is small (initialize,
//! tools/list, tools/call, ping) so the implementation is intentionally
//! direct: no SDK pull-in, no transport abstraction.

pub mod jsonrpc;
pub mod openai_compat;
pub mod progress;
pub mod projection;
pub mod render;
pub mod state;
pub mod timefmt;
pub mod tools;
pub mod transport;

pub use progress::ProgressSink;
pub use state::McpState;
pub use transport::{mcp_router, mcp_router_with_auth, BearerAuthCheck};
