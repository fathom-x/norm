//! Minimal ActionCable subscriber for the marketplace's per-order
//! `payment_status:<id>` channel.
//!
//! The order page's own WebSocket topic carries the streaming preview —
//! `{action: "partial", seq, delta[, content]}` frames per seller flush,
//! plus `{action: "refresh"}` on fulfillment transitions — and the Rails
//! `PaymentChannel` subscribes anonymously: the order UUID is the
//! credential, exactly as it is for the order page itself. Subscribing
//! here turns the `/v1` proxy's 1 Hz poll-and-diff into push, with the
//! poll demoted to a safety net.
//!
//! Deliberately minimal: no reconnect loop (a consumer that loses the
//! socket falls back to polling, which is always correct), no pong
//! handling beyond what tungstenite does itself (ActionCable pings are
//! text frames used for liveness, not protocol pings), and any parse
//! surprise degrades to `Closed` — fail-to-poll, never fail the request.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

/// One frame off the order's cable topic, pre-parsed to what the `/v1`
/// streaming path needs.
#[derive(Debug, Clone, PartialEq)]
pub enum CableFrame {
    /// A streaming-preview frame. `delta` is the newly-flushed chunk;
    /// `content`, when present, is the full accumulated buffer (the
    /// marketplace's periodic resync frames, and every frame from a
    /// marketplace still broadcasting the old full-buffer protocol).
    Partial {
        seq: u64,
        delta: Option<String>,
        content: Option<String>,
    },
    /// Something about the order changed (fulfillment transition, quote,
    /// …) — worth a poll now rather than at the next tick.
    Refresh,
    /// The subscription is gone (socket closed, error, or the server
    /// rejected it). No more frames will arrive; fall back to polling.
    Closed,
}

/// How long to wait for the TCP+TLS+HTTP upgrade before giving up and
/// letting the caller poll. First-token latency must never wait on a
/// marketplace that has no reachable cable endpoint.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// Subscribe to `payment_status:<order_id>`. Returns a receiver of
/// [`CableFrame`]s; the socket is driven by a spawned task that ends —
/// after sending [`CableFrame::Closed`] — when the connection drops, the
/// receiver is dropped, or the server disconnects. Errors before the
/// socket is established are returned directly so the caller can decide
/// to poll instead.
pub async fn subscribe_payment_status(
    base_url: &str,
    order_id: &str,
) -> Result<mpsc::Receiver<CableFrame>, String> {
    let ws_url = cable_url(base_url)?;
    let connect = tokio_tungstenite::connect_async(&ws_url);
    let (stream, _resp) = tokio::time::timeout(CONNECT_TIMEOUT, connect)
        .await
        .map_err(|_| format!("cable connect timed out after {CONNECT_TIMEOUT:?}"))?
        .map_err(|e| format!("cable connect failed: {e}"))?;

    let identifier = serde_json::to_string(&serde_json::json!({
        "channel": "PaymentChannel",
        "order_id": order_id,
    }))
    .map_err(|e| format!("identifier encode: {e}"))?;

    let (tx, rx) = mpsc::channel(64);
    tokio::spawn(pump(stream, identifier, tx));
    Ok(rx)
}

/// Derive `ws(s)://<host>/cable` from the marketplace base URL. The
/// mount path is Rails' default and matches what the marketplace's own
/// pages and bot clients use.
fn cable_url(base_url: &str) -> Result<String, String> {
    let mut url = reqwest::Url::parse(base_url).map_err(|e| format!("base url: {e}"))?;
    let scheme = match url.scheme() {
        "https" => "wss",
        "http" => "ws",
        other => return Err(format!("unsupported scheme {other:?}")),
    };
    url.set_scheme(scheme)
        .map_err(|_| "scheme rewrite failed".to_string())?;
    url.set_path("/cable");
    url.set_query(None);
    Ok(url.to_string())
}

async fn pump(
    stream: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    identifier: String,
    tx: mpsc::Sender<CableFrame>,
) {
    let (mut sink, mut source) = stream.split();
    let mut subscribed = false;

    loop {
        // Watch for the consumer dropping alongside the socket read. The
        // only other place we notice a gone receiver is a failed
        // `tx.send`, which is reached solely for *data* frames — and once
        // an order delivers, the topic carries nothing but ActionCable's
        // periodic keep-alive pings. Without this arm the task would spin
        // on those pings forever, leaking a socket, a task, and a
        // server-side cable connection for every streamed turn.
        let msg = tokio::select! {
            biased;
            _ = tx.closed() => break,
            msg = source.next() => match msg {
                Some(Ok(m)) => m,
                Some(Err(_)) | None => break,
            },
        };
        let text = match msg {
            Message::Text(t) => t,
            Message::Close(_) => break,
            // Binary/Ping/Pong frames: tungstenite answers protocol pings
            // itself; ActionCable's own pings arrive as Text.
            _ => continue,
        };
        let value: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue,
        };

        match value.get("type").and_then(Value::as_str) {
            Some("welcome") => {
                let cmd = serde_json::json!({
                    "command": "subscribe",
                    "identifier": identifier,
                })
                .to_string();
                if sink.send(Message::Text(cmd)).await.is_err() {
                    break;
                }
            }
            Some("confirm_subscription") => subscribed = true,
            Some("reject_subscription") | Some("disconnect") => break,
            Some(_) => {} // ping et al.
            None => {
                if !subscribed {
                    continue;
                }
                let Some(frame) = parse_data_frame(&value) else {
                    continue;
                };
                if tx.send(frame).await.is_err() {
                    return; // receiver gone — no Closed needed
                }
            }
        }
    }
    let _ = tx.send(CableFrame::Closed).await;
}

/// Parse one ActionCable data message (`{"identifier": …, "message": …}`).
/// `None` for shapes we don't care about.
fn parse_data_frame(value: &Value) -> Option<CableFrame> {
    let message = value.get("message")?;
    match message.get("action").and_then(Value::as_str) {
        Some("partial") => Some(CableFrame::Partial {
            seq: message.get("seq").and_then(Value::as_u64)?,
            delta: message
                .get("delta")
                .and_then(Value::as_str)
                .map(str::to_string),
            content: message
                .get("content")
                .and_then(Value::as_str)
                .map(str::to_string),
        }),
        Some(_) => Some(CableFrame::Refresh),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cable_url_rewrites_scheme_and_path() {
        assert_eq!(
            cable_url("https://overpay.example.com").unwrap(),
            "wss://overpay.example.com/cable"
        );
        assert_eq!(
            cable_url("http://127.0.0.1:3000/api?x=1").unwrap(),
            "ws://127.0.0.1:3000/cable"
        );
    }

    #[test]
    fn parses_partial_and_refresh_frames() {
        let partial: Value = serde_json::from_str(
            r#"{"identifier":"i","message":{"action":"partial","seq":3,"delta":"abc"}}"#,
        )
        .unwrap();
        assert_eq!(
            parse_data_frame(&partial),
            Some(CableFrame::Partial {
                seq: 3,
                delta: Some("abc".into()),
                content: None
            })
        );

        let legacy: Value = serde_json::from_str(
            r#"{"identifier":"i","message":{"action":"partial","seq":2,"content":"ab"}}"#,
        )
        .unwrap();
        assert_eq!(
            parse_data_frame(&legacy),
            Some(CableFrame::Partial {
                seq: 2,
                delta: None,
                content: Some("ab".into())
            })
        );

        let refresh: Value =
            serde_json::from_str(r#"{"identifier":"i","message":{"action":"refresh"}}"#).unwrap();
        assert_eq!(parse_data_frame(&refresh), Some(CableFrame::Refresh));

        let ping: Value = serde_json::from_str(r#"{"type":"ping","message":123}"#).unwrap();
        assert!(parse_data_frame(&ping).is_none());
    }
}
