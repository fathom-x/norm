//! Per-wallet order cache — local mirror of delivered/terminal orders.
//!
//! Each wallet's cached orders live as plaintext JSON files under its per-`npub`
//! state directory: `<data dir>/<npub>/orders/<order_id>.json` (issue #310).
//! One file per order, written atomically (temp + rename), so concurrent
//! upserts of different orders never race on a shared file.
//!
//! Plaintext for now (deliberately *not* encrypted to the wallet key — unlike
//! the other artifacts in the per-wallet dir): the order cache is regenerable
//! from the Rails API via `sync_purchases`, and keeping it clear avoids
//! requiring an unlocked DB just to render the dashboard. The full Rails order
//! payload is kept under `snapshot` so tools don't have to re-fetch large
//! blobs.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::{wallet_state, DbError, Result};

/// One cached order. `snapshot` is the full Rails order payload;
/// `delivered_content_schema` is the parsed listing schema (if any).
/// Serialized field names match the Python `_purchase_row_to_dict` output so
/// the MCP/dashboard layers see byte-identical JSON — and the same shape is
/// what's persisted on disk (round-trips via `Deserialize`).
#[derive(Debug, Clone, Serialize, Deserialize)]
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

impl PurchaseRow {
    /// Sort key matching the SQL `COALESCE(delivered_at, paid_at, cached_at)`.
    fn order_key(&self) -> i64 {
        self.delivered_at.or(self.paid_at).unwrap_or(self.cached_at)
    }
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

/// `<base>/<npub>/orders`.
fn orders_dir(base: &Path, npub: &str) -> Result<PathBuf> {
    Ok(wallet_state::wallet_dir_in(base, npub)?.join("orders"))
}

/// Path to one order's JSON file, or `None` if `order_id` isn't a safe
/// filename component.
fn order_path(base: &Path, npub: &str, order_id: &str) -> Result<Option<PathBuf>> {
    if !wallet_state::is_safe_component(order_id) {
        return Ok(None);
    }
    Ok(Some(
        orders_dir(base, npub)?.join(format!("{order_id}.json")),
    ))
}

/// Build a [`PurchaseRow`] from a Rails order payload. Mirrors the column
/// extraction in the former SQL `upsert`.
fn row_from_order(order: &Value, order_id: String, now: i64) -> PurchaseRow {
    let listing = order.get("listing").filter(|v| v.is_object());
    let seller = order.get("seller").filter(|v| v.is_object());
    let schema = listing
        .and_then(|l| l.get("delivered_content_schema"))
        .filter(|v| !v.is_null())
        .cloned();

    PurchaseRow {
        order_id,
        listing_id: s(order, "listing_id").or_else(|| listing.and_then(|l| s(l, "id"))),
        title: s(order, "product_title")
            .or_else(|| s(order, "title"))
            .or_else(|| listing.and_then(|l| s(l, "title"))),
        seller: s(order, "seller_slug")
            .or_else(|| s(order, "seller_username"))
            .or_else(|| seller.and_then(|x| s(x, "username"))),
        payment_status: s(order, "status").or_else(|| s(order, "payment_status")),
        fulfillment_status: s(order, "fulfillment_status"),
        delivered_at: coerce_unix_secs(order.get("delivered_at")),
        paid_at: coerce_unix_secs(order.get("paid_at")),
        total_usd_cents: coerce_unix_secs(order.get("total_usd_cents")),
        delivered_content: s(order, "delivered_content"),
        delivered_content_type: s(order, "delivered_content_type"),
        delivered_content_schema: schema,
        cached_at: now,
        snapshot: order.clone(),
    }
}

/// Store/refresh a cached order for a wallet. `order` is the Rails order
/// payload (already unwrapped from any `{data: …}` envelope). Returns the
/// order_id on success, or `None` if the payload has no id (or the id can't be
/// used as a filename). Mirrors `upsert_purchase` in `wallet_mcp/db.py`.
pub(crate) fn upsert(base: &Path, npub: &str, order: &Value, now: i64) -> Result<Option<String>> {
    if !order.is_object() {
        return Ok(None);
    }
    let order_id = match s(order, "order_id").or_else(|| s(order, "id")) {
        Some(id) => id,
        None => return Ok(None),
    };
    let Some(path) = order_path(base, npub, &order_id)? else {
        return Ok(None);
    };

    let row = row_from_order(order, order_id.clone(), now);
    let bytes = serde_json::to_vec(&row)?;

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, &path)?;
    Ok(Some(order_id))
}

/// Read and parse every cached order for `npub`. Missing dir → empty.
fn read_all(base: &Path, npub: &str) -> Result<Vec<PurchaseRow>> {
    let dir = orders_dir(base, npub)?;
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(DbError::Io(e)),
    };
    let mut rows = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue; // skip stray *.json.tmp or unrelated files
        }
        let bytes = std::fs::read(&path)?;
        match serde_json::from_slice::<PurchaseRow>(&bytes) {
            Ok(row) => rows.push(row),
            // A corrupt cache entry shouldn't sink the whole listing; the
            // cache is regenerable via sync_purchases.
            Err(_) => continue,
        }
    }
    Ok(rows)
}

/// Cached orders for a wallet, newest first
/// (`COALESCE(delivered_at, paid_at, cached_at)` descending), optionally
/// filtered by `fulfillment_status`, then `offset`/`limit` applied.
pub(crate) fn list(
    base: &Path,
    npub: &str,
    limit: i64,
    offset: i64,
    fulfillment_status: Option<&str>,
) -> Result<Vec<PurchaseRow>> {
    let mut rows = read_all(base, npub)?;
    if let Some(fs) = fulfillment_status {
        rows.retain(|r| r.fulfillment_status.as_deref() == Some(fs));
    }
    rows.sort_by_key(|r| std::cmp::Reverse(r.order_key()));

    let offset = offset.max(0) as usize;
    let mut out: Vec<PurchaseRow> = rows.into_iter().skip(offset).collect();
    if limit >= 0 {
        out.truncate(limit as usize);
    }
    Ok(out)
}

pub(crate) fn read(base: &Path, npub: &str, order_id: &str) -> Result<Option<PurchaseRow>> {
    let Some(path) = order_path(base, npub, order_id)? else {
        return Ok(None);
    };
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(DbError::Io(e)),
    };
    Ok(Some(serde_json::from_slice(&bytes)?))
}

pub(crate) fn delete(base: &Path, npub: &str, order_id: &str) -> Result<()> {
    let Some(path) = order_path(base, npub, order_id)? else {
        return Ok(());
    };
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(DbError::Io(e)),
    }
}

pub(crate) fn count(base: &Path, npub: &str) -> Result<i64> {
    Ok(read_all(base, npub)?.len() as i64)
}
