//! Server→client progress streaming for `tools/call`.
//!
//! The Streamable-HTTP transport (`transport.rs`) can answer a
//! `tools/call` with an SSE stream instead of a single JSON body. While a
//! tool runs it may push intermediate `notifications/progress` messages
//! through a [`ProgressSink`]; the transport forwards each as an SSE event
//! and then closes the stream with the tool's final JSON-RPC response.
//!
//! Progress is opt-in per the MCP spec: the client signals interest by
//! sending `params._meta.progressToken` on the request, and every
//! notification echoes that token. When no token was supplied the sink is
//! inert (`emit` is a no-op), so tools can call it unconditionally.

use serde_json::{json, Map, Value};
use tokio::sync::mpsc::UnboundedSender;

/// Sink a tool handler uses to publish incremental progress. Cheap to
/// clone; dropping it (or the receiver) just stops delivery.
#[derive(Clone)]
pub struct ProgressSink {
    tx: UnboundedSender<Value>,
    /// The client-supplied `progressToken`. `None` → the client didn't opt
    /// in, so [`emit`](ProgressSink::emit) drops every notification.
    token: Option<Value>,
}

impl ProgressSink {
    /// Build a sink bound to `token` (from `params._meta.progressToken`).
    pub fn new(tx: UnboundedSender<Value>, token: Option<Value>) -> Self {
        Self { tx, token }
    }

    /// Whether the client opted into progress. Handlers can skip building a
    /// payload entirely when this is false.
    pub fn wants_progress(&self) -> bool {
        self.token.is_some()
    }

    /// Emit one `notifications/progress` message.
    ///
    /// * `progress` — a monotonically increasing counter (ticks, bytes,
    ///   tokens — whatever the tool is measuring).
    /// * `total` — the expected final value when known.
    /// * `message` — a short human-readable status line.
    /// * `data` — optional structured payload for programmatic clients;
    ///   pass [`Value::Null`] to omit it.
    ///
    /// No-op when the client didn't supply a `progressToken`, or once the
    /// receiving stream has been dropped.
    pub fn emit(&self, progress: u64, total: Option<u64>, message: impl Into<String>, data: Value) {
        let Some(token) = &self.token else { return };

        let mut params = Map::new();
        params.insert("progressToken".into(), token.clone());
        params.insert("progress".into(), json!(progress));
        if let Some(total) = total {
            params.insert("total".into(), json!(total));
        }
        params.insert("message".into(), json!(message.into()));
        if !data.is_null() {
            params.insert("data".into(), data);
        }

        let note = json!({
            "jsonrpc": "2.0",
            "method": "notifications/progress",
            "params": Value::Object(params),
        });
        // Best-effort: if the client hung up, the receiver is gone and the
        // send fails — nothing we can (or should) do about it.
        let _ = self.tx.send(note);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_well_formed_notification_when_token_present() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let sink = ProgressSink::new(tx, Some(json!("tok-1")));
        assert!(sink.wants_progress());

        sink.emit(2, Some(5), "halfway", json!({"order_id": "O1"}));

        let note = rx.try_recv().expect("a notification was queued");
        assert_eq!(note["jsonrpc"], "2.0");
        assert_eq!(note["method"], "notifications/progress");
        assert_eq!(note["params"]["progressToken"], "tok-1");
        assert_eq!(note["params"]["progress"], 2);
        assert_eq!(note["params"]["total"], 5);
        assert_eq!(note["params"]["message"], "halfway");
        assert_eq!(note["params"]["data"]["order_id"], "O1");
    }

    #[test]
    fn omits_total_and_data_when_absent() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let sink = ProgressSink::new(tx, Some(json!(7)));

        sink.emit(1, None, "tick", Value::Null);

        let note = rx.try_recv().unwrap();
        assert!(note["params"].get("total").is_none());
        assert!(note["params"].get("data").is_none());
        assert_eq!(note["params"]["progressToken"], 7);
    }

    #[test]
    fn is_inert_without_a_token() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let sink = ProgressSink::new(tx, None);
        assert!(!sink.wants_progress());

        sink.emit(1, None, "ignored", Value::Null);

        assert!(rx.try_recv().is_err(), "no notification should be queued");
    }
}
