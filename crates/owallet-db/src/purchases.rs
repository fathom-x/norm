//! `purchases` table — local mirror of delivered/terminal orders per wallet.
//!
//! Ports the purchase-cache functions from `wallet_mcp/db.py`. Keyed by
//! `(npub, order_id)` so multiple wallets in one DB stay isolated.
//! `snapshot_json` holds the full Rails order payload (including
//! `delivered_content`) so tools don't have to re-fetch large blobs;
//! `delivered_content` is also broken out into its own column. Plaintext —
//! consistent with the existing unencrypted wallet metadata (address,
//! username); the DB file itself sits behind the master password.

use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::Serialize;
use serde_json::Value;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::Result;

/// One cached purchase. `snapshot` is the full Rails order payload;
/// `delivered_content_schema` is the parsed listing schema (if any).
/// Serialized field names match the Python `_purchase_row_to_dict` output
/// so the MCP/dashboard layers see byte-identical JSON.
#[derive(Debug, Clone, Serialize)]
pub struct PurchaseRow {
    pub order_id: String,
    pub listing_id: Option<String>,
    pub title: Option<String>,
    pub seller: Option<String>,
    pub payment_status: Option<String>,
    pub fulfillment_status: Option<String>,
    pub delivered_at: Option<i64>,
    pub paid_at: Option<i64>,
    pub total_usd_cents: Option<i64>,
    pub delivered_content: Option<String>,
    pub delivered_content_type: Option<String>,
    pub delivered_content_schema: Option<Value>,
    pub cached_at: i64,
    pub snapshot: Value,
}

/// Coerce a JSON value into unix seconds. Accepts an integer, a numeric
/// string, or an ISO-8601 / RFC-3339 timestamp (`…Z` or `…+00:00`).
/// Mirrors `_coerce_int` in `wallet_mcp/db.py`.
fn coerce_unix_secs(v: Option<&Value>) -> Option<i64> {
    match v {
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Some(Value::String(s)) => {
            if s.is_empty() {
                return None;
            }
            if let Ok(n) = s.parse::<i64>() {
                return Some(n);
            }
            OffsetDateTime::parse(s, &Rfc3339)
                .ok()
                .map(OffsetDateTime::unix_timestamp)
        }
        _ => None,
    }
}

/// Read a string field from a JSON object.
fn s(o: &Value, k: &str) -> Option<String> {
    o.get(k).and_then(Value::as_str).map(str::to_string)
}

/// Store/refresh a cached order for a wallet. `order` is the Rails order
/// payload (already unwrapped from any `{data: …}` envelope). Returns the
/// order_id on success, or `None` if the payload has no id. Mirrors
/// `upsert_purchase` in `wallet_mcp/db.py`.
pub(crate) fn upsert(
    conn: &Connection,
    npub: &str,
    order: &Value,
    now: i64,
) -> Result<Option<String>> {
    if !order.is_object() {
        return Ok(None);
    }
    let order_id = match s(order, "order_id").or_else(|| s(order, "id")) {
        Some(id) => id,
        None => return Ok(None),
    };

    let listing = order.get("listing").filter(|v| v.is_object());
    let seller = order.get("seller").filter(|v| v.is_object());
    let schema = listing.and_then(|l| l.get("delivered_content_schema"));

    let listing_id = s(order, "listing_id").or_else(|| listing.and_then(|l| s(l, "id")));
    let title = s(order, "product_title")
        .or_else(|| s(order, "title"))
        .or_else(|| listing.and_then(|l| s(l, "title")));
    let seller_str = s(order, "seller_slug")
        .or_else(|| s(order, "seller_username"))
        .or_else(|| seller.and_then(|x| s(x, "username")));
    let payment_status = s(order, "status").or_else(|| s(order, "payment_status"));
    let fulfillment_status = s(order, "fulfillment_status");
    let delivered_at = coerce_unix_secs(order.get("delivered_at"));
    let paid_at = coerce_unix_secs(order.get("paid_at"));
    let total_usd_cents = coerce_unix_secs(order.get("total_usd_cents"));
    let delivered_content = s(order, "delivered_content");
    let delivered_content_type = s(order, "delivered_content_type");
    let schema_json = schema
        .filter(|v| !v.is_null())
        .map(serde_json::to_string)
        .transpose()?;
    let snapshot_json = serde_json::to_string(order)?;

    conn.execute(
        "INSERT INTO purchases(
             npub, order_id, listing_id, title, seller, payment_status,
             fulfillment_status, delivered_at, paid_at, total_usd_cents,
             delivered_content, delivered_content_type, schema_json,
             snapshot_json, cached_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)
         ON CONFLICT(npub, order_id) DO UPDATE SET
             listing_id=excluded.listing_id,
             title=excluded.title,
             seller=excluded.seller,
             payment_status=excluded.payment_status,
             fulfillment_status=excluded.fulfillment_status,
             delivered_at=excluded.delivered_at,
             paid_at=excluded.paid_at,
             total_usd_cents=excluded.total_usd_cents,
             delivered_content=excluded.delivered_content,
             delivered_content_type=excluded.delivered_content_type,
             schema_json=excluded.schema_json,
             snapshot_json=excluded.snapshot_json,
             cached_at=excluded.cached_at",
        params![
            npub,
            order_id,
            listing_id,
            title,
            seller_str,
            payment_status,
            fulfillment_status,
            delivered_at,
            paid_at,
            total_usd_cents,
            delivered_content,
            delivered_content_type,
            schema_json,
            snapshot_json,
            now,
        ],
    )?;
    Ok(Some(order_id))
}

fn row_to_purchase(row: &Row) -> rusqlite::Result<PurchaseRow> {
    let snapshot_json: String = row.get("snapshot_json")?;
    let schema_json: Option<String> = row.get("schema_json")?;
    Ok(PurchaseRow {
        order_id: row.get("order_id")?,
        listing_id: row.get("listing_id")?,
        title: row.get("title")?,
        seller: row.get("seller")?,
        payment_status: row.get("payment_status")?,
        fulfillment_status: row.get("fulfillment_status")?,
        delivered_at: row.get("delivered_at")?,
        paid_at: row.get("paid_at")?,
        total_usd_cents: row.get("total_usd_cents")?,
        delivered_content: row.get("delivered_content")?,
        delivered_content_type: row.get("delivered_content_type")?,
        delivered_content_schema: schema_json
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok()),
        cached_at: row.get("cached_at")?,
        snapshot: serde_json::from_str(&snapshot_json).unwrap_or(Value::Null),
    })
}

/// Cached purchases for a wallet, newest first
/// (`COALESCE(delivered_at, paid_at, cached_at) DESC`).
pub(crate) fn list(
    conn: &Connection,
    npub: &str,
    limit: i64,
    offset: i64,
    fulfillment_status: Option<&str>,
) -> Result<Vec<PurchaseRow>> {
    let mut sql = String::from("SELECT * FROM purchases WHERE npub = ?1");
    if fulfillment_status.is_some() {
        sql.push_str(" AND fulfillment_status = ?2");
    }
    sql.push_str(" ORDER BY COALESCE(delivered_at, paid_at, cached_at) DESC LIMIT ? OFFSET ?");

    let mut stmt = conn.prepare(&sql)?;
    let rows = if let Some(fs) = fulfillment_status {
        stmt.query_map(params![npub, fs, limit, offset], row_to_purchase)?
            .collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        stmt.query_map(params![npub, limit, offset], row_to_purchase)?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    Ok(rows)
}

pub(crate) fn read(conn: &Connection, npub: &str, order_id: &str) -> Result<Option<PurchaseRow>> {
    let row = conn
        .query_row(
            "SELECT * FROM purchases WHERE npub = ?1 AND order_id = ?2",
            params![npub, order_id],
            row_to_purchase,
        )
        .optional()?;
    Ok(row)
}

pub(crate) fn delete(conn: &Connection, npub: &str, order_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM purchases WHERE npub = ?1 AND order_id = ?2",
        params![npub, order_id],
    )?;
    Ok(())
}

pub(crate) fn count(conn: &Connection, npub: &str) -> Result<i64> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM purchases WHERE npub = ?1",
        params![npub],
        |r| r.get(0),
    )?;
    Ok(n)
}
