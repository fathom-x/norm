//! `/wallet/purchases` — local purchase cache UI.
//!
//! Ports the three Python routes in `wallet_mcp/server.py:1017-1135`:
//! a list view, a `POST …/sync` that backfills from Overpay (reusing the
//! `sync_purchases` MCP tool), and a per-order detail view that renders
//! `delivered_content`. The HTML follows the Rust dashboard's askama
//! conventions rather than copying Python's inline-CSS strings; the
//! behaviour (rows, badges, sync button, content rendering) matches.

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use owallet_db::PurchaseRow;
use serde::Deserialize;
use serde_json::{json, Value};
use time::macros::format_description;
use time::OffsetDateTime;

use super::{current_session, redirect_to_login};
use crate::error::AppError;
use crate::session::SessionRole;
use crate::state::AppState;
use crate::templates::{PurchaseDetailTemplate, PurchaseListRow, PurchasesListTemplate};

/// Resolve the npub whose purchases we show: a wallet session sees its own,
/// an admin session sees the default wallet.
fn active_npub(
    state: &AppState,
    session: &crate::session::WebSession,
) -> Result<Option<String>, AppError> {
    match &session.role {
        SessionRole::Wallet { npub } => Ok(Some(npub.clone())),
        SessionRole::Admin => {
            let db = state
                .db
                .lock()
                .map_err(|e| AppError::Internal(format!("db mutex poisoned: {e}")))?;
            Ok(db.read_default_npub()?)
        }
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct PurchasesQuery {
    #[serde(default)]
    pub notice: Option<String>,
    #[serde(default)]
    pub count: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

pub async fn list_get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<PurchasesQuery>,
) -> Result<Response, AppError> {
    let Some(session) = current_session(&state.sessions, &headers) else {
        return Ok(redirect_to_login().into_response());
    };
    let Some(npub) = active_npub(&state, &session)? else {
        return Ok(
            Html("<h2>No active wallet. Select one first.</h2>".to_string()).into_response(),
        );
    };

    let (purchases, count) = {
        let db = state
            .db
            .lock()
            .map_err(|e| AppError::Internal(format!("db mutex poisoned: {e}")))?;
        (
            db.list_purchases(&npub, 200, 0, None)?,
            db.count_purchases(&npub)?,
        )
    };

    let rows: Vec<PurchaseListRow> = purchases
        .iter()
        .map(|p| {
            let (badge_class, status_label) = status_badge(p.fulfillment_status.as_deref());
            PurchaseListRow {
                order_id: p.order_id.clone(),
                title: p.title.clone().unwrap_or_else(|| p.order_id.clone()),
                seller: p.seller.clone().unwrap_or_else(|| "—".into()),
                badge_class,
                status_label,
                amount: format_dollars(p.total_usd_cents),
                when: format_timestamp(p.delivered_at.or(p.paid_at).or(Some(p.cached_at))),
            }
        })
        .collect();

    let (notice, notice_is_error) = match q.notice.as_deref() {
        Some("synced") => (
            Some(format!(
                "Synced {} order(s) from Overpay.",
                q.count.as_deref().unwrap_or("?")
            )),
            false,
        ),
        Some("sync_error") => (
            Some(format!(
                "Sync failed: {}",
                q.error.as_deref().unwrap_or("Unknown error")
            )),
            true,
        ),
        _ => (None, false),
    };

    let tpl = PurchasesListTemplate {
        npub_short: shorten_npub(&npub),
        count,
        notice,
        notice_is_error,
        rows,
    };
    Ok(Html(tpl.render()?).into_response())
}

pub async fn sync_post(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let Some(session) = current_session(&state.sessions, &headers) else {
        return Ok(redirect_to_login().into_response());
    };
    let Some(npub) = active_npub(&state, &session)? else {
        return Ok(
            Redirect::to("/wallet/purchases?notice=sync_error&error=no+wallet").into_response(),
        );
    };

    // Reuse the `sync_purchases` MCP tool bound to the active wallet.
    let mcp = owallet_mcp::McpState::new(
        state.db.clone(),
        state.overpay.clone(),
        state.host_key.clone(),
    )
    .with_evm(state.evm.rpc_url.clone(), state.evm.network.clone())
    .with_npub(Some(npub));

    let result: Value = match owallet_mcp::tools::dispatch(&mcp, "sync_purchases", json!({})).await
    {
        Ok(owallet_mcp::tools::ToolOutput::Json(v)) => v,
        Ok(_) => json!({}),
        Err(e) => json!({ "error": e.to_string() }),
    };

    if let Some(err) = result.get("error").and_then(Value::as_str) {
        let enc = urlencoding(err);
        return Ok(
            Redirect::to(&format!("/wallet/purchases?notice=sync_error&error={enc}"))
                .into_response(),
        );
    }
    let synced = result.get("synced").and_then(Value::as_u64).unwrap_or(0);
    Ok(Redirect::to(&format!("/wallet/purchases?notice=synced&count={synced}")).into_response())
}

pub async fn detail_get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(order_id): Path<String>,
) -> Result<Response, AppError> {
    let Some(session) = current_session(&state.sessions, &headers) else {
        return Ok(redirect_to_login().into_response());
    };
    let Some(npub) = active_npub(&state, &session)? else {
        return Ok(Html("<h2>No active wallet.</h2>".to_string()).into_response());
    };

    let record = {
        let db = state
            .db
            .lock()
            .map_err(|e| AppError::Internal(format!("db mutex poisoned: {e}")))?;
        db.read_purchase(&npub, &order_id)?
    };
    let Some(record) = record else {
        return Ok(Html(
            "<h2>Order not in local cache.</h2>\
             <p><a href=\"/wallet/purchases\">Back</a> — try syncing from Overpay.</p>"
                .to_string(),
        )
        .into_response());
    };

    let (badge_class, status_label) = status_badge(record.fulfillment_status.as_deref());
    let meta = vec![
        (
            "Seller".into(),
            record.seller.clone().unwrap_or_else(|| "—".into()),
        ),
        (
            "Payment status".into(),
            record.payment_status.clone().unwrap_or_else(|| "—".into()),
        ),
        (
            "Fulfillment status".into(),
            record
                .fulfillment_status
                .clone()
                .unwrap_or_else(|| "—".into()),
        ),
        ("Amount".into(), format_dollars(record.total_usd_cents)),
        ("Paid at".into(), format_timestamp(record.paid_at)),
        ("Delivered at".into(), format_timestamp(record.delivered_at)),
        ("Order ID".into(), record.order_id.clone()),
    ];

    let tpl = PurchaseDetailTemplate {
        title: record
            .title
            .clone()
            .unwrap_or_else(|| format!("Order {}", record.order_id)),
        badge_class,
        status_label,
        meta,
        content_html: render_delivered_content(&record),
    };
    Ok(Html(tpl.render()?).into_response())
}

// ---------------------------------------------------------------------------
// Formatting helpers (ports of _status_badge / _format_dollars /
// _format_purchase_timestamp / _render_delivered_content)
// ---------------------------------------------------------------------------

fn shorten_npub(npub: &str) -> String {
    if npub.len() > 24 {
        format!("{}…{}", &npub[..14], &npub[npub.len() - 6..])
    } else {
        npub.to_string()
    }
}

/// `(css class, label)` for a fulfillment status badge.
fn status_badge(status: Option<&str>) -> (String, String) {
    let s = status.unwrap_or("—");
    let class = match s {
        "delivered" => "badge-delivered",
        "failed" => "badge-failed",
        "cancelled" => "badge-cancelled",
        "shipping" => "badge-shipping",
        _ => "badge-other",
    };
    (class.to_string(), s.to_string())
}

fn format_dollars(cents: Option<i64>) -> String {
    match cents {
        Some(c) => format!("${:.2}", c as f64 / 100.0),
        None => "—".to_string(),
    }
}

fn format_timestamp(ts: Option<i64>) -> String {
    match ts.and_then(|t| OffsetDateTime::from_unix_timestamp(t).ok()) {
        Some(dt) => {
            let fmt = format_description!("[year]-[month]-[day] [hour]:[minute] UTC");
            dt.format(&fmt).unwrap_or_else(|_| "—".to_string())
        }
        None => "—".to_string(),
    }
}

/// Minimal HTML escape (the analogue of Python's `html.escape`).
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(c),
        }
    }
    out
}

fn img_src(value: &str, mime: &str, encoding: &str) -> String {
    if value.starts_with("data:") || value.starts_with("http://") || value.starts_with("https://") {
        value.to_string()
    } else if encoding == "base64" {
        format!("data:{mime};base64,{value}")
    } else {
        value.to_string()
    }
}

fn field(label: &str, body: &str) -> String {
    format!(
        "<div class=\"delivered-field\"><p class=\"delivered-field-label\">{}</p>\
         <div class=\"delivered-field-body\">{}</div></div>",
        esc(label),
        body
    )
}

/// Render `delivered_content` as escaped HTML. Mirrors the 4-tier logic in
/// `_render_delivered_content`: JSON+schema (per-property `x-widget`),
/// legacy `{description, image}` envelope, inline image content-types, and
/// markdown / plain / `<pre>` fallback.
fn render_delivered_content(record: &PurchaseRow) -> String {
    let Some(content) = record.delivered_content.as_deref() else {
        return String::new();
    };
    if content.is_empty() {
        return String::new();
    }
    let ctype = record
        .delivered_content_type
        .as_deref()
        .unwrap_or("text/plain");
    let schema = record
        .delivered_content_schema
        .as_ref()
        .filter(|v| v.is_object());

    // 1. JSON content + listing schema → render each declared property.
    if ctype == "application/json" {
        if let Some(schema) = schema {
            if let Some(props) = schema.get("properties").and_then(Value::as_object) {
                if let Ok(Value::Object(parsed)) = serde_json::from_str::<Value>(content) {
                    let mut parts = String::new();
                    for (key, prop) in props {
                        let Some(value) = parsed.get(key) else {
                            continue;
                        };
                        if value.is_null() || is_empty_value(value) {
                            continue;
                        }
                        let widget = prop.get("x-widget").and_then(Value::as_str);
                        let label = prop
                            .get("title")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .unwrap_or_else(|| titleize(key));
                        let body = render_widget(widget, prop, value);
                        parts.push_str(&field(&label, &body));
                    }
                    if !parts.is_empty() {
                        return parts;
                    }
                }
            }
        }
        // 2. JSON content w/o usable schema → {description, image} envelope.
        if let Ok(Value::Object(parsed)) = serde_json::from_str::<Value>(content) {
            let mut parts = String::new();
            if let Some(img) = parsed.get("image").and_then(Value::as_str) {
                let mime = parsed
                    .get("image_mime_type")
                    .and_then(Value::as_str)
                    .unwrap_or("image/png");
                let src = img_src(img, mime, "base64");
                parts.push_str(&format!(
                    "<img src=\"{}\" class=\"delivered-image\" loading=\"lazy\" alt=\"Delivered image\">",
                    esc(&src)
                ));
            }
            if let Some(desc) = parsed.get("description").and_then(Value::as_str) {
                parts.push_str(&format!("<p class=\"delivered-text\">{}</p>", esc(desc)));
            }
            if !parts.is_empty() {
                return parts;
            }
            return format!(
                "<pre class=\"delivered-code\">{}</pre>",
                esc(&serde_json::to_string_pretty(&parsed).unwrap_or_default())
            );
        }
    }

    // 3. Inline image content type.
    if ctype == "image/png" || ctype == "image/jpeg" {
        return format!(
            "<img src=\"{}\" class=\"delivered-image\" loading=\"lazy\" alt=\"Delivered image\">",
            esc(content)
        );
    }

    // 4. Plain text / markdown / fallback.
    if ctype == "text/markdown" || content.contains('\n') || content.len() > 200 {
        format!("<pre class=\"delivered-code\">{}</pre>", esc(content))
    } else {
        format!("<p class=\"delivered-text\">{}</p>", esc(content))
    }
}

fn render_widget(widget: Option<&str>, prop: &Value, value: &Value) -> String {
    match widget {
        Some("image") => {
            let mime = prop
                .get("x-mime-type")
                .and_then(Value::as_str)
                .unwrap_or("image/png");
            let encoding = prop
                .get("x-encoding")
                .and_then(Value::as_str)
                .unwrap_or("base64");
            let raw = value.as_str().unwrap_or("");
            let src = img_src(raw, mime, encoding);
            format!(
                "<img src=\"{}\" class=\"delivered-image\" loading=\"lazy\" alt=\"image\">",
                esc(&src)
            )
        }
        Some("code") | Some("json") | Some("textarea") | Some("html") => {
            let body = match value {
                Value::Object(_) | Value::Array(_) => {
                    serde_json::to_string_pretty(value).unwrap_or_default()
                }
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            format!("<pre class=\"delivered-code\">{}</pre>", esc(&body))
        }
        Some("link") | Some("url") if value.is_string() => {
            let url = value.as_str().unwrap_or("");
            format!(
                "<p class=\"delivered-link\"><a href=\"{}\" target=\"_blank\" \
                 rel=\"noopener noreferrer nofollow\">{}</a></p>",
                esc(url),
                esc(url)
            )
        }
        _ => match value {
            Value::Object(_) | Value::Array(_) => format!(
                "<pre class=\"delivered-code\">{}</pre>",
                esc(&serde_json::to_string_pretty(value).unwrap_or_default())
            ),
            Value::Bool(b) => format!(
                "<p class=\"delivered-text\">{}</p>",
                if *b { "Yes" } else { "No" }
            ),
            _ => {
                let s = match value {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                if s.contains('\n') || s.len() > 160 {
                    format!("<pre class=\"delivered-code\">{}</pre>", esc(&s))
                } else {
                    format!("<p class=\"delivered-text\">{}</p>", esc(&s))
                }
            }
        },
    }
}

fn is_empty_value(v: &Value) -> bool {
    match v {
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
        _ => false,
    }
}

fn titleize(key: &str) -> String {
    key.split('_')
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn urlencoding(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
