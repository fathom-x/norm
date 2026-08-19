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

    let (purchases, count, timezone) = {
        let db = state
            .db
            .lock()
            .map_err(|e| AppError::Internal(format!("db mutex poisoned: {e}")))?;
        (
            db.list_purchases(&npub, 200, 0, None)?,
            db.count_purchases(&npub)?,
            db.read_timezone()?,
        )
    };

    let tz = owallet_mcp::timefmt::wallet_tz(timezone.as_deref());
    let now = OffsetDateTime::now_utc();
    let rows: Vec<PurchaseListRow> = purchases
        .iter()
        .map(|p| {
            let (badge_class, status_label) = status_badge(p.fulfillment_status.as_deref());
            let when_ts = p.delivered_at.or(p.paid_at).or(Some(p.cached_at));
            PurchaseListRow {
                order_id: p.order_id.clone(),
                title: p.title.clone().unwrap_or_else(|| p.order_id.clone()),
                seller: p.seller.clone().unwrap_or_else(|| "—".into()),
                badge_class,
                status_label,
                amount: format_dollars(p.total_usd_cents),
                when: owallet_mcp::timefmt::format_in_tz(when_ts, tz),
                when_age: owallet_mcp::timefmt::relative_age(when_ts, now),
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
    let mcp = owallet_mcp::McpState::new(state.db.clone(), state.overpay.clone())
        .with_legacy_host_keys(state.legacy_host_keys.clone())
        .with_evm(state.evm.rpc_url.clone(), state.evm.network.clone())
        .with_npub(Some(npub));

    let result: Value =
        match owallet_mcp::tools::dispatch(&mcp, "sync_purchases", json!({}), None).await {
            Ok(out) => out.data,
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

    let (record, timezone) = {
        let db = state
            .db
            .lock()
            .map_err(|e| AppError::Internal(format!("db mutex poisoned: {e}")))?;
        (db.read_purchase(&npub, &order_id)?, db.read_timezone()?)
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
    let tz = owallet_mcp::timefmt::wallet_tz(timezone.as_deref());
    let now = OffsetDateTime::now_utc();
    let timestamp_row = |label: &str, ts: Option<i64>| {
        (
            label.to_string(),
            owallet_mcp::timefmt::format_in_tz(ts, tz),
            owallet_mcp::timefmt::relative_age(ts, now),
        )
    };
    let meta = vec![
        (
            "Seller".into(),
            record.seller.clone().unwrap_or_else(|| "—".into()),
            String::new(),
        ),
        (
            "Payment status".into(),
            record.payment_status.clone().unwrap_or_else(|| "—".into()),
            String::new(),
        ),
        (
            "Fulfillment status".into(),
            record
                .fulfillment_status
                .clone()
                .unwrap_or_else(|| "—".into()),
            String::new(),
        ),
        (
            "Amount".into(),
            format_dollars(record.total_usd_cents),
            String::new(),
        ),
        timestamp_row("Paid at", record.paid_at),
        timestamp_row("Delivered at", record.delivered_at),
        ("Order ID".into(), record.order_id.clone(), String::new()),
    ];

    let tpl = PurchaseDetailTemplate {
        title: record
            .title
            .clone()
            .unwrap_or_else(|| format!("Order {}", record.order_id)),
        badge_class,
        status_label,
        meta,
        buyer_input_html: render_buyer_note(&record.snapshot),
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

/// Render the order's `buyer_note` (what the buyer submitted — a prompt,
/// code to run, shipping notes, …) out of the cached snapshot. Schema-driven
/// listings store it as a JSON object serialized to a string, so that case
/// renders one labeled block per field; anything else renders as plain
/// escaped text. Empty string when the snapshot carries no note.
fn render_buyer_note(snapshot: &Value) -> String {
    let note = snapshot.get("buyer_note");
    let (obj, text) = match note {
        None | Some(Value::Null) => return String::new(),
        Some(Value::Object(map)) => (Some(map.clone()), None),
        Some(Value::String(s)) if s.trim().is_empty() => return String::new(),
        Some(Value::String(s)) => match serde_json::from_str::<Value>(s) {
            Ok(Value::Object(map)) => (Some(map), None),
            _ => (None, Some(s.clone())),
        },
        Some(other) => (None, Some(other.to_string())),
    };

    if let Some(map) = obj {
        let mut out = String::new();
        for (key, value) in &map {
            // Orders placed through the /v1 provider carry OpenAI-shaped
            // fields; recognize them by shape (not key) so any listing whose
            // note contains a chat history or tool definitions benefits.
            if let Value::Array(items) = value {
                if let Some(html) = render_chat_messages(items) {
                    out.push_str(&field(key, &html));
                    continue;
                }
                if let Some(html) = render_tool_defs(items) {
                    out.push_str(&field(key, &html));
                    continue;
                }
            }
            let body = match value {
                Value::String(s) if s.contains('\n') => {
                    format!("<pre class=\"delivered-code\">{}</pre>", esc(s))
                }
                Value::String(s) => format!("<p class=\"delivered-text\">{}</p>", esc(s)),
                other => format!(
                    "<pre class=\"delivered-code\">{}</pre>",
                    esc(&serde_json::to_string_pretty(other).unwrap_or_default())
                ),
            };
            out.push_str(&field(key, &body));
        }
        return out;
    }

    let text = text.unwrap_or_default();
    if text.contains('\n') {
        format!("<pre class=\"delivered-code\">{}</pre>", esc(&text))
    } else {
        format!("<p class=\"delivered-text\">{}</p>", esc(&text))
    }
}

/// An OpenAI-style chat history (`[{role, content}, …]`) rendered as a
/// transcript: one block per message, labeled by role, with assistant
/// `tool_calls` shown as `name(arguments)`. `None` unless every element has
/// a string `role` — the caller then falls back to raw JSON.
fn render_chat_messages(items: &[Value]) -> Option<String> {
    if items.is_empty()
        || !items
            .iter()
            .all(|m| m.get("role").and_then(Value::as_str).is_some())
    {
        return None;
    }
    let mut out = String::new();
    for msg in items {
        let role = msg.get("role").and_then(Value::as_str).unwrap_or("?");
        let mut body = String::new();
        match msg.get("content") {
            Some(Value::String(s)) if !s.is_empty() => {
                body.push_str(&format!(
                    "<pre class=\"delivered-code\" style=\"margin:0;\">{}</pre>",
                    esc(s)
                ));
            }
            // Content-parts form: [{type: "text"|"input_text", text}, …] —
            // join the text parts, fall back to JSON for exotic parts.
            Some(Value::Array(parts)) => {
                let text: Vec<&str> = parts
                    .iter()
                    .filter_map(|p| p.get("text").and_then(Value::as_str))
                    .collect();
                if text.is_empty() {
                    body.push_str(&format!(
                        "<pre class=\"delivered-code\" style=\"margin:0;\">{}</pre>",
                        esc(&serde_json::to_string_pretty(parts).unwrap_or_default())
                    ));
                } else {
                    body.push_str(&format!(
                        "<pre class=\"delivered-code\" style=\"margin:0;\">{}</pre>",
                        esc(&text.join("\n"))
                    ));
                }
            }
            _ => {}
        }
        if let Some(Value::Array(calls)) = msg.get("tool_calls") {
            for call in calls {
                let f = call.get("function");
                let name = f
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("tool");
                let args = f
                    .and_then(|f| f.get("arguments"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                body.push_str(&format!(
                    "<pre class=\"delivered-code\" style=\"margin:0;\">→ {}({})</pre>",
                    esc(name),
                    esc(args)
                ));
            }
        }
        if body.is_empty() {
            body.push_str("<p class=\"delivered-text\">—</p>");
        }
        out.push_str(&format!(
            "<div style=\"margin-bottom:10px;\">\
             <p class=\"delivered-field-label\" style=\"margin-bottom:2px;\">{}</p>{}</div>",
            esc(role),
            body
        ));
    }
    Some(out)
}

/// OpenAI-style function-tool definitions rendered as a compact list —
/// `name` + description up front, the JSON-schema `parameters` folded into a
/// `<details>` block (native HTML, no JS). `None` unless every element is a
/// function definition.
fn render_tool_defs(items: &[Value]) -> Option<String> {
    let defs: Vec<(&str, Option<&str>, Option<&Value>)> = items
        .iter()
        .map(|t| {
            let f = t.get("function").unwrap_or(t);
            (
                f.get("name").and_then(Value::as_str),
                f.get("description").and_then(Value::as_str),
                f.get("parameters"),
            )
        })
        .map(|(name, desc, params)| name.map(|n| (n, desc, params)))
        .collect::<Option<_>>()?;
    if defs.is_empty() {
        return None;
    }
    let mut out = String::new();
    for (name, desc, params) in defs {
        out.push_str(&format!(
            "<p class=\"delivered-text\" style=\"margin-bottom:4px;\"><code>{}</code>{}</p>",
            esc(name),
            match desc {
                Some(d) => format!(" — {}", esc(d)),
                None => String::new(),
            }
        ));
        if let Some(p) = params {
            out.push_str(&format!(
                "<details style=\"margin-bottom:10px;\"><summary class=\"muted\" \
                 style=\"cursor:pointer; font-size:.8rem;\">parameters</summary>\
                 <pre class=\"delivered-code\">{}</pre></details>",
                esc(&serde_json::to_string_pretty(p).unwrap_or_default())
            ));
        }
    }
    Some(out)
}

/// Render `delivered_content` as escaped HTML. Mirrors the 4-tier logic in
/// `_render_delivered_content`: JSON+schema (per-property `x-widget`),
/// legacy `{description, image}` envelope, inline image content-types, and
/// markdown / plain / `<pre>` fallback.
fn render_delivered_content(record: &PurchaseRow) -> String {
    if let Some(url) = record.delivered_content_url.as_deref() {
        return format!(
            "<a href=\"{}\" class=\"delivered-url\" target=\"_blank\" rel=\"noopener noreferrer\">{}</a>",
            esc(url),
            esc(url)
        );
    }
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
                    // serde_json's map iterates alphabetically (the listing's
                    // declared order is lost in the cache), which files
                    // metadata like `credits_refunded` above the actual
                    // deliverable. Render content-bearing widgets first so
                    // the output leads and the scalars trail it.
                    let is_primary = |prop: &Value| {
                        matches!(
                            prop.get("x-widget").and_then(Value::as_str),
                            Some("markdown" | "image" | "code" | "textarea" | "html")
                        )
                    };
                    let ordered = props
                        .iter()
                        .filter(|(_, prop)| is_primary(prop))
                        .chain(props.iter().filter(|(_, prop)| !is_primary(prop)));
                    let mut parts = String::new();
                    for (key, prop) in ordered {
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
            // Hosted-image alternative to inline `image` bytes: bots that
            // deliver a URL (weather_reporter's `image_url`) rather than
            // base64. Without the listing schema in the cache (older rows,
            // or an order payload that didn't carry it) this envelope is
            // the only chance to show it — http(s) only, anything else
            // would render a broken <img>.
            if let Some(url) = parsed
                .get("image_url")
                .and_then(Value::as_str)
                .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
            {
                parts.push_str(&format!(
                    "<img src=\"{}\" class=\"delivered-image\" loading=\"lazy\" alt=\"Delivered image\">",
                    esc(url)
                ));
            }
            if let Some(desc) = parsed.get("description").and_then(Value::as_str) {
                parts.push_str(&format!(
                    "<div class=\"delivered-markdown\">{}</div>",
                    markdown_to_html(desc)
                ));
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

/// Markdown → sanitized HTML. The source is seller-authored, so raw
/// HTML blocks/inlines are demoted to escaped text (pulldown-cmark's writer
/// escapes `Text` events), and link/image destinations outside
/// http(s)/mailto are dropped to `#` — markdown formatting comes through,
/// markup injection does not.
fn markdown_to_html(md: &str) -> String {
    use pulldown_cmark::{html, CowStr, Event, Options, Parser, Tag};

    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    let safe_url = |url: &str| {
        let lower = url.trim().to_ascii_lowercase();
        lower.starts_with("http://")
            || lower.starts_with("https://")
            || lower.starts_with("mailto:")
    };
    let events = Parser::new_ext(md, options).map(|event| match event {
        Event::Html(s) | Event::InlineHtml(s) => Event::Text(s),
        Event::Start(Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Link {
            link_type,
            dest_url: if safe_url(&dest_url) {
                dest_url
            } else {
                CowStr::from("#")
            },
            title,
            id,
        }),
        Event::Start(Tag::Image {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Image {
            link_type,
            dest_url: if safe_url(&dest_url) {
                dest_url
            } else {
                CowStr::from("#")
            },
            title,
            id,
        }),
        other => other,
    });
    let mut out = String::new();
    html::push_html(&mut out, events);
    out
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
        Some("markdown") if value.is_string() => {
            format!(
                "<div class=\"delivered-markdown\">{}</div>",
                markdown_to_html(value.as_str().unwrap_or(""))
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

#[cfg(test)]
mod render_tests {
    use super::*;

    fn row(content: &str, schema: Option<Value>) -> PurchaseRow {
        PurchaseRow {
            order_id: "O1".into(),
            listing_id: None,
            title: None,
            seller: None,
            payment_status: Some("paid".into()),
            fulfillment_status: Some("delivered".into()),
            delivered_at: None,
            paid_at: None,
            total_usd_cents: Some(5),
            delivered_content: Some(content.to_string()),
            delivered_content_url: None,
            delivered_content_type: Some("application/json".into()),
            delivered_content_schema: schema,
            cached_at: 0,
            snapshot: serde_json::json!({}),
        }
    }

    const WEATHER: &str = r##"{"description":"# Weather Report\nSunny, 22C","image_url":"https://img.example/w.png"}"##;

    #[test]
    fn schemaless_json_renders_a_hosted_image_url_and_markdown_description() {
        // The weather_reporter regression: cached rows predating the
        // schema-in-order-payload fix have no delivered_content_schema, so
        // the {description, image_url} envelope is all we have to go on.
        let html = render_delivered_content(&row(WEATHER, None));
        assert!(
            html.contains(r#"<img src="https://img.example/w.png""#),
            "hosted image must render: {html}"
        );
        assert!(
            html.contains("delivered-markdown") && html.contains("Sunny, 22C"),
            "description renders as markdown: {html}"
        );
    }

    #[test]
    fn schemaless_image_url_must_be_http_to_render() {
        let html = render_delivered_content(&row(
            r#"{"description":"x","image_url":"javascript:alert(1)"}"#,
            None,
        ));
        assert!(
            !html.contains("<img"),
            "non-http url must not render: {html}"
        );
    }

    #[test]
    fn schema_driven_image_widget_renders_a_url_valued_field() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "description": {"type": "string", "x-widget": "markdown"},
                "image_url":   {"type": "string", "x-widget": "image"},
            }
        });
        let html = render_delivered_content(&row(WEATHER, Some(schema)));
        assert!(
            html.contains(r#"<img src="https://img.example/w.png""#),
            "schema image widget with a URL value: {html}"
        );
    }
}
