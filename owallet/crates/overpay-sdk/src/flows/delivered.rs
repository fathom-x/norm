use serde_json::{json, Value};

use crate::auth::Auth;
use crate::delivery::DeliveredContent;
use crate::error::Error;
use crate::Client;

/// Inline an offloaded deliverable into an order snapshot (the raw
/// `{"data": {...}}` body, or the bare order).
///
/// Once an order is delivered, the marketplace may hand back a
/// `delivered_content_url` instead of `delivered_content`. Text behind the
/// link is fetched and put in `delivered_content`; a binary file stays a
/// link, with `delivered_content_type` filled in when missing and its size
/// in `delivered_content_bytes`. Undelivered and already-inline snapshots
/// are left alone.
pub async fn resolve_delivered(client: &Client, snapshot: &mut Value) -> Result<(), Error> {
    let data = if snapshot.get("data").is_some() {
        &mut snapshot["data"]
    } else {
        snapshot
    };
    let delivered = data.get("fulfillment_status").and_then(Value::as_str) == Some("delivered");
    let inline = data
        .get("delivered_content")
        .and_then(Value::as_str)
        .is_some();
    if !delivered || inline {
        return Ok(());
    }
    let Some(url) = data
        .get("delivered_content_url")
        .and_then(Value::as_str)
        .filter(|u| !u.is_empty())
        .map(str::to_string)
    else {
        return Ok(());
    };
    match client.fetch_delivered_content(&url).await? {
        DeliveredContent::Text(content) => data["delivered_content"] = json!(content),
        DeliveredContent::Binary {
            content_type,
            bytes,
        } => {
            if let Some(ct) = content_type {
                if data
                    .get("delivered_content_type")
                    .and_then(Value::as_str)
                    .is_none()
                {
                    data["delivered_content_type"] = json!(ct);
                }
            }
            let api_size = data
                .get("delivered_content_byte_size")
                .and_then(Value::as_u64);
            if let Some(n) = bytes.or(api_size) {
                data["delivered_content_bytes"] = json!(n);
            }
        }
    }
    Ok(())
}

/// `GET /api/v1/orders/{id}` with an offloaded deliverable inlined — see
/// [`resolve_delivered`]. Returns the raw body.
pub async fn get_order_resolved(
    client: &Client,
    order_id: &str,
    auth: Auth<'_>,
) -> Result<Value, Error> {
    let mut snapshot = client.orders().get(order_id, auth).await?.into_raw();
    resolve_delivered(client, &mut snapshot).await?;
    Ok(snapshot)
}
