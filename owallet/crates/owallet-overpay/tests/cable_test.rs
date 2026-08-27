//! Drives `cable::subscribe_payment_status` against a real WebSocket
//! server speaking the ActionCable handshake — welcome, subscribe,
//! confirm, data frames — since nothing short of a live socket exercises
//! the split-sink pump.

use futures_util::{SinkExt, StreamExt};
use owallet_overpay::cable::{subscribe_payment_status, CableFrame};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

/// One-connection ActionCable server: performs the handshake, asserts
/// the subscribe identifier targets PaymentChannel with the expected
/// order id, then sends `frames` and closes.
async fn cable_server(expected_order: &'static str, frames: Vec<Value>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        let (mut sink, mut source) = ws.split();

        sink.send(Message::Text(json!({"type": "welcome"}).to_string()))
            .await
            .unwrap();

        // The subscribe command: identifier is a JSON *string*.
        let sub = loop {
            match source.next().await.unwrap().unwrap() {
                Message::Text(t) => break t,
                _ => continue,
            }
        };
        let sub: Value = serde_json::from_str(&sub).unwrap();
        assert_eq!(sub["command"], "subscribe");
        let identifier: Value =
            serde_json::from_str(sub["identifier"].as_str().unwrap()).unwrap();
        assert_eq!(identifier["channel"], "PaymentChannel");
        assert_eq!(identifier["order_id"], expected_order);

        sink.send(Message::Text(
            json!({"type": "confirm_subscription", "identifier": sub["identifier"]}).to_string(),
        ))
        .await
        .unwrap();

        for frame in frames {
            sink.send(Message::Text(frame.to_string())).await.unwrap();
        }
        // Dropping the socket ends the stream — the client must emit Closed.
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn subscribes_and_delivers_frames_in_order_then_closed() {
    let base = cable_server(
        "order-ws-1",
        vec![
            json!({"type": "ping", "message": 123}),
            json!({"identifier": "i", "message": {"action": "partial", "seq": 1, "delta": "Hel", "content": "Hel"}}),
            json!({"identifier": "i", "message": {"action": "partial", "seq": 2, "delta": "lo"}}),
            json!({"identifier": "i", "message": {"action": "refresh"}}),
        ],
    )
    .await;

    let mut rx = subscribe_payment_status(&base, "order-ws-1").await.unwrap();

    assert_eq!(
        rx.recv().await,
        Some(CableFrame::Partial {
            seq: 1,
            delta: Some("Hel".into()),
            content: Some("Hel".into())
        })
    );
    assert_eq!(
        rx.recv().await,
        Some(CableFrame::Partial {
            seq: 2,
            delta: Some("lo".into()),
            content: None
        })
    );
    assert_eq!(rx.recv().await, Some(CableFrame::Refresh));
    assert_eq!(rx.recv().await, Some(CableFrame::Closed));
    assert_eq!(rx.recv().await, None);
}

#[tokio::test]
async fn connect_failure_is_an_error_not_a_hang() {
    // Nothing listens on this port (bind-then-drop reserves then frees it).
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let started = std::time::Instant::now();
    let result = subscribe_payment_status(&format!("http://{addr}"), "order-x").await;
    assert!(result.is_err());
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "must fail fast, not hang past the connect timeout"
    );
}
