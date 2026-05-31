//! Human/model-readable rendering of tool results (fathom-x/overpay#295).
//!
//! Every MCP tool handler returns a `serde_json::Value` (the raw Rails
//! payload or a small composed dict). The transport puts that value in
//! `structuredContent` for programmatic clients, but the model only ever
//! sees the `content` text blocks — so dumping pretty-printed JSON there
//! both burns context and fails to steer the model toward its next call.
//!
//! This module turns each shape into a concise summary plus a one-line
//! `Next:` instruction (the pattern GitHub's MCP uses after a merge).
//! Renderers are **total**: any missing/unexpected field degrades to a
//! placeholder rather than panicking, because Rails shapes drift and a
//! formatting bug must never turn a successful call into an error.

use std::fmt::Write as _;

use serde_json::Value;

use crate::tools::ToolError;

/// Render a tool's result `Value` into the model-facing text block.
/// `tool` is the MCP tool name so we can pick the right shape renderer.
pub fn render(tool: &str, data: &Value) -> String {
    // Soft errors: several tools return `Ok({"error": ...})` to keep a
    // partial result (order_url, order_id) useful. Render those uniformly
    // before dispatching to the happy-path renderer.
    if let Some(text) = render_soft_error(tool, data) {
        return text;
    }
    match tool {
        "get_account_info" => render_account(data),
        "list_marketplace" => render_listings(data),
        "get_listing" => render_listing(data),
        "get_wallet_orders" => render_orders(data),
        "create_order" => render_order(tool, data),
        "get_order_status" => render_order(tool, data),
        "wait_for_order" => render_order(tool, data),
        "get_merchant_credits" => render_credits(data),
        "redeem_merchant_credits" => render_redeem(data),
        "buy" => render_buy(data),
        "send_usdc" => render_send(data),
        "list_purchases" => render_purchases(data),
        "get_purchase" => render_purchase(data),
        "sync_purchases" => render_sync(data),
        // Unknown tool name: never happens (dispatch already rejected it),
        // but stay total and fall back to compact JSON.
        _ => compact(data),
    }
}

/// Render a [`ToolError`] into a friendly, actionable message: the
/// underlying error text (callers/tests rely on the existing substrings)
/// followed by a one-line next step.
pub fn render_error(e: &ToolError) -> String {
    let base = e.to_string();
    let hint = match e {
        ToolError::NoWallet => {
            "Next: run `owallet select` (or pass a wallet identifier), then retry."
        }
        ToolError::NotAuthorized => {
            "Next: run `owallet authorize` to link this wallet to Overpay, then retry."
        }
        ToolError::MissingArg(_) | ToolError::InvalidArg { .. } => {
            "Next: fix the argument shown above and call the tool again."
        }
        ToolError::WaitTimeout { .. } => {
            "Next: call wait_for_order again to keep polling, or get_order_status for a one-shot check."
        }
        ToolError::Overpay(_) => {
            "Next: verify the order/listing id and that the wallet is authorized, then retry."
        }
        ToolError::Evm(_) => {
            "Next: check the recipient address, chain, and that the wallet holds enough USDC + gas."
        }
        ToolError::NotImplemented => "Next: this action isn't available in this build.",
        ToolError::Internal(_) => "Next: this is an internal error — retry; if it persists, report it.",
    };
    format!("⚠️ {base}\n{hint}")
}

// ---------------------------------------------------------------------------
// Soft-error rendering (Ok payloads carrying an `error` key)
// ---------------------------------------------------------------------------

/// If `data` is an object with a string `error` field, render it as a
/// warning plus the most useful next step we can infer from the
/// accompanying fields. Returns `None` for normal payloads.
fn render_soft_error(tool: &str, data: &Value) -> Option<String> {
    let obj = data.as_object()?;
    let err = obj.get("error").and_then(Value::as_str)?;

    // get_purchase's not_cached miss is a routine "fetch it first" nudge,
    // not really a failure.
    if err == "not_cached" {
        let oid = obj.get("order_id").and_then(Value::as_str).unwrap_or("?");
        return Some(format!(
            "ℹ️ Order {oid} isn't in the local cache yet.\nNext: call get_order_status(order_id=\"{oid}\") or wait_for_order to populate it, then get_purchase again."
        ));
    }

    let mut out = format!("⚠️ {err}");
    if let Some(oid) = obj.get("order_id").and_then(Value::as_str) {
        let _ = write!(out, "\nOrder: {oid}");
    }
    if let Some(url) = obj.get("order_url").and_then(Value::as_str) {
        let _ = write!(out, "\nPay via web checkout: {url}");
    }
    if let Some(hint) = obj.get("hint").and_then(Value::as_str) {
        let _ = write!(out, "\nNext: {hint}");
    } else if tool == "buy" {
        let _ = write!(out, "\nNext: open the order_url to complete payment.");
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// Per-shape renderers
// ---------------------------------------------------------------------------

/// `get_account_info`: the wallet identity + balances table. This is the
/// markdown that used to be built inline in `tools::get_account_info`.
fn render_account(data: &Value) -> String {
    let s = |k: &str| data.get(k).and_then(Value::as_str).unwrap_or("—");
    let account_data = data.get("account").and_then(|a| a.get("data"));
    let username = account_data
        .and_then(|d| d.get("username").and_then(Value::as_str))
        .or_else(|| data.get("account_hint").and_then(Value::as_str))
        .unwrap_or("—");
    let account_number = account_data
        .and_then(|d| d.get("formatted_account_number").and_then(Value::as_str))
        .unwrap_or("—");

    // Human chain name (e.g. "Base") for the balance cells, derived from
    // the CAIP-2 network; falls back to the raw network string.
    let network = s("network");
    let chain_name = owallet_evm::chains::from_caip2(network)
        .map(|c| c.name.to_string())
        .unwrap_or_else(|_| network.to_string());

    // Balance cells fall back to the formatted balance, then a balance
    // error, then "unavailable" — matching the prior inline logic.
    let bal = |key: &str, symbol: &str| -> String {
        if let Some(f) = data
            .get(key)
            .and_then(|v| v.get("formatted").and_then(Value::as_str))
        {
            format!("{f} {symbol} ({chain_name})")
        } else if let Some(e) = data.get("balance_error").and_then(Value::as_str) {
            e.to_string()
        } else {
            "unavailable".to_string()
        }
    };

    let mut md = String::from("| Field | Value |\n|---|---|\n");
    let _ = writeln!(md, "| Address | {} |", s("address"));
    let _ = writeln!(md, "| Network | {} |", s("network"));
    let _ = writeln!(md, "| npub | {} |", s("npub"));
    let _ = writeln!(md, "| ETH Balance | {} |", bal("eth_balance", "ETH"));
    let _ = writeln!(md, "| USDC Balance | {} |", bal("usdc_balance", "USDC"));
    let _ = writeln!(md, "| Username | {username} |");
    let _ = write!(md, "| Account Number | {account_number} |");
    md
}

/// `list_marketplace`: numbered listing summary + browse/buy steer.
fn render_listings(data: &Value) -> String {
    let items = data.get("data").and_then(Value::as_array);
    let Some(items) = items else {
        return format!(
            "Marketplace returned an unexpected shape.\n{}",
            compact(data)
        );
    };
    if items.is_empty() {
        return "No listings matched. Next: relax the category/seller filters and call list_marketplace again.".to_string();
    }

    let mut out = format!("Found {} listing(s):\n", items.len());
    for (i, l) in items.iter().enumerate() {
        let id = field_str(l, &["id", "listing_id"]);
        let title = field_str(l, &["title", "name"]);
        let seller = field_str(l, &["seller_slug", "seller"]);
        let price = price_cell(l);
        let _ = write!(out, "{}. {title} · {id} · {price}", i + 1);
        if seller != "—" {
            let _ = write!(out, " · @{seller}");
        }
        if let Some(eta) = l.get("delivery_eta_seconds").and_then(Value::as_i64) {
            let _ = write!(out, " · ~{}", human_duration(eta));
        }
        out.push('\n');
    }
    if let Some(c) = data.get("next_cursor").and_then(Value::as_str) {
        let _ = writeln!(
            out,
            "More available — pass cursor=\"{c}\" for the next page."
        );
    }
    out.push_str("Next: call get_listing(listing_id) to see its buyer_note_schema, then create_order(listing_id).");
    out
}

/// `get_listing`: single listing + whether a structured buyer_note is required.
fn render_listing(data: &Value) -> String {
    let inner = data.get("data").unwrap_or(data);
    let id = field_str(inner, &["id", "listing_id"]);
    let title = field_str(inner, &["title", "name"]);
    let seller = field_str(inner, &["seller_slug", "seller"]);
    let price = price_cell(inner);

    let mut out = format!("Listing {id}: {title}\nPrice: {price}");
    if seller != "—" {
        let _ = write!(out, " · seller @{seller}");
    }
    out.push('\n');

    let schema = inner.get("buyer_note_schema");
    let has_schema = matches!(schema, Some(Value::Object(m)) if !m.is_empty());
    if has_schema {
        let schema = schema.unwrap();
        if let Some(req) = schema.get("required").and_then(Value::as_array) {
            let fields: Vec<&str> = req.iter().filter_map(Value::as_str).collect();
            if !fields.is_empty() {
                let _ = writeln!(
                    out,
                    "Requires a structured buyer_note with: {}",
                    fields.join(", ")
                );
            }
        }
        out.push_str("Next: build a buyer_note matching buyer_note_schema (in structuredContent), then call create_order(listing_id, buyer_note).");
    } else {
        out.push_str("Next: call create_order(listing_id) — a free-form buyer_note is optional.");
    }
    out
}

/// `get_wallet_orders`: compact one-line-per-order list.
fn render_orders(data: &Value) -> String {
    let items = data.get("data").and_then(Value::as_array);
    let Some(items) = items else {
        return format!("Orders returned an unexpected shape.\n{}", compact(data));
    };
    if items.is_empty() {
        return "No orders found for this wallet. Next: browse with list_marketplace, then create_order.".to_string();
    }
    let mut out = format!("{} order(s):\n", items.len());
    for o in items {
        let id = field_str(o, &["order_id", "id"]);
        let pay = field_str(o, &["payment_status"]);
        let ful = field_str(o, &["fulfillment_status"]);
        let total = price_cell(o);
        let _ = writeln!(out, "• {id} · payment={pay} · fulfillment={ful} · {total}");
    }
    if let Some(c) = data.get("next_cursor").and_then(Value::as_str) {
        let _ = writeln!(
            out,
            "More available — pass cursor=\"{c}\" for the next page."
        );
    }
    out.push_str("Next: get_order_status(order_id) for detail, or wait_for_order to block until one is delivered.");
    out
}

/// `create_order` / `get_order_status` / `wait_for_order`: single order
/// snapshot. The next step is keyed off the order's status.
fn render_order(tool: &str, data: &Value) -> String {
    let order = data.get("data").unwrap_or(data);
    let id = field_str(order, &["order_id", "id"]);
    let pay = field_str(order, &["payment_status"]);
    let ful = field_str(order, &["fulfillment_status"]);
    let total = price_cell(order);

    let mut out = format!("Order {id}\nPayment: {pay} · Fulfillment: {ful} · {total}");

    if let Some(t) = order.get("tracking_number").and_then(Value::as_str) {
        let carrier = order.get("tracking_carrier").and_then(Value::as_str);
        match carrier {
            Some(c) => {
                let _ = write!(out, "\nTracking: {t} ({c})");
            }
            None => {
                let _ = write!(out, "\nTracking: {t}");
            }
        }
    }

    // Stripped large delivered_content leaves a pointer dict behind.
    if let Some(ptr) = order.get("delivered_content_cached") {
        let size = ptr.get("size_bytes").and_then(Value::as_i64).unwrap_or(0);
        let _ = write!(
            out,
            "\nDelivered content ({size} bytes) cached locally — fetch with get_purchase(order_id=\"{id}\")."
        );
    } else if let Some(c) = order.get("delivered_content").and_then(Value::as_str) {
        // Small inline content: show it directly.
        let _ = write!(out, "\nDelivered content: {}", truncate(c, 500));
    }

    // wait_for_order splices these on; surface the poll outcome.
    if tool == "wait_for_order" {
        let waited = data.get("waited_seconds").and_then(Value::as_i64);
        let timed_out = data.get("timed_out").and_then(Value::as_bool);
        if let (Some(w), Some(to)) = (waited, timed_out) {
            if to {
                let _ = write!(out, "\nTimed out after {w}s (status not yet reached).");
            } else {
                let _ = write!(out, "\nReached after {w}s.");
            }
        }
    }

    let _ = write!(out, "\n{}", order_next_step(id, pay, ful, order));
    out
}

/// Pick the most useful next action for an order given its statuses.
fn order_next_step(id: &str, pay: &str, ful: &str, order: &Value) -> String {
    if ful == "delivered" {
        if order.get("delivered_content_cached").is_some() {
            return format!("Next: get_purchase(order_id=\"{id}\") to read the delivered content.");
        }
        return "Next: done — the order is delivered.".to_string();
    }
    if ful == "failed" || ful == "cancelled" {
        return "Next: the order won't complete; create a new order or contact the seller."
            .to_string();
    }
    if pay == "pending" {
        // Awaiting payment.
        if let Some(addr) = order.get("payment_address").and_then(Value::as_str) {
            return format!(
                "Next: send the quoted USDC to {addr} with send_usdc, or pay via the order page."
            );
        }
        return format!("Next: pay this order, then wait_for_order(order_id=\"{id}\").");
    }
    // Paid but still being fulfilled.
    format!("Next: wait_for_order(order_id=\"{id}\", until_status=\"delivered\") to block until it ships.")
}

/// `get_merchant_credits`: either a single seller's balance or a list.
fn render_credits(data: &Value) -> String {
    if let Some(list) = data.get("data").and_then(Value::as_array) {
        if list.is_empty() {
            return "No merchant credits yet. Next: buy(seller_slug, amount_usd) to fund a balance.".to_string();
        }
        let mut out = String::from("Merchant credit balances:\n");
        for c in list {
            let _ = writeln!(
                out,
                "• @{} · {}",
                field_str(c, &["seller_slug", "seller"]),
                balance_cell(c)
            );
        }
        out.push_str(
            "Next: redeem_merchant_credits(seller_slug, order_id) to apply a balance to an order.",
        );
        return out;
    }
    // Single-seller shape (flat object).
    let seller = field_str(data, &["seller_slug", "seller"]);
    format!(
        "Credit with @{seller}: {}\nNext: redeem_merchant_credits(seller_slug=\"{seller}\", order_id) to spend it.",
        balance_cell(data)
    )
}

/// `redeem_merchant_credits`: amount applied + remaining balance.
fn render_redeem(data: &Value) -> String {
    let status = field_str(data, &["status"]);
    let applied = money_field(data, &["amount_redeemed_cents"]);
    let remaining = money_field(data, &["credit_balance_cents", "remaining_balance_cents"]);
    let mut out = format!("✅ Credits applied ({status}).");
    if let Some(a) = applied {
        let _ = write!(out, "\nRedeemed: {a}");
    }
    if let Some(r) = remaining {
        let _ = write!(out, "\nRemaining balance: {r}");
    }
    if let Some(msg) = data.get("message").and_then(Value::as_str) {
        let _ = write!(out, "\n{msg}");
    }
    out.push_str("\nNext: get_order_status(order_id) to confirm the order is now settled.");
    out
}

/// `buy`: success path (soft-error path handled in `render_soft_error`).
fn render_buy(data: &Value) -> String {
    let order_id = field_str(data, &["order_id"]);
    let amount = data.get("payment_amount_usdc").and_then(Value::as_f64);
    let tx = data.get("tx_hash").and_then(Value::as_str);

    let mut out = String::from("✅ Payment sent");
    let _ = write!(out, " · order {order_id}");
    if let Some(a) = amount {
        let _ = write!(out, " · {} USDC", trim_float(a));
    }
    if let Some(h) = tx {
        let _ = write!(out, " · tx {}", short_hash(h));
    }
    if let Some(note) = data.get("note").and_then(Value::as_str) {
        let _ = write!(out, "\n{note}");
    }
    let _ = write!(
        out,
        "\nNext: wait_for_order(order_id=\"{order_id}\", until_status=\"delivered\") to confirm the credits land."
    );
    out
}

/// `send_usdc`: just a tx hash.
fn render_send(data: &Value) -> String {
    match data.get("tx_hash").and_then(Value::as_str) {
        Some(h) => format!(
            "✅ USDC transfer broadcast · tx {}\nNext: the recipient/order settles automatically once the transfer confirms on-chain.",
            short_hash(h)
        ),
        None => format!("Transfer submitted.\n{}", compact(data)),
    }
}

/// `list_purchases`: count + compact rows.
fn render_purchases(data: &Value) -> String {
    let count = data.get("count").and_then(Value::as_i64).unwrap_or(0);
    let rows = data.get("purchases").and_then(Value::as_array);
    if count == 0 || rows.map(|r| r.is_empty()).unwrap_or(true) {
        return "No cached purchases. Next: get_order_status/wait_for_order on an order, or sync_purchases to backfill from Overpay.".to_string();
    }
    let mut out = format!("{count} cached purchase(s):\n");
    if let Some(rows) = rows {
        for p in rows {
            let id = field_str(p, &["order_id", "id"]);
            let title = field_str(p, &["title"]);
            let ful = field_str(p, &["fulfillment_status"]);
            let _ = writeln!(out, "• {id} · {title} · {ful}");
        }
    }
    out.push_str("Next: get_purchase(order_id) to read a purchase's delivered content.");
    out
}

/// `get_purchase`: a cached order (not_cached handled as a soft error).
fn render_purchase(data: &Value) -> String {
    let id = field_str(data, &["order_id", "id"]);
    let title = field_str(data, &["title"]);
    let ful = field_str(data, &["fulfillment_status"]);
    let mut out = format!("Purchase {id}: {title} · {ful}");
    match data.get("delivered_content").and_then(Value::as_str) {
        Some(c) if !c.is_empty() => {
            let _ = write!(out, "\nDelivered content:\n{}", truncate(c, 2000));
        }
        _ => {
            out.push_str("\n(No delivered content stored. Full payload is in structuredContent.)");
        }
    }
    out
}

/// `sync_purchases`: count synced + any errors.
fn render_sync(data: &Value) -> String {
    let synced = data.get("synced").and_then(Value::as_i64).unwrap_or(0);
    let errors = data.get("errors").and_then(Value::as_array);
    let err_count = errors.map(|e| e.len()).unwrap_or(0);
    let mut out = format!("Synced {synced} purchase(s) from Overpay.");
    if err_count > 0 {
        let _ = write!(out, "\n{err_count} error(s):");
        for e in errors.unwrap().iter().take(5) {
            if let Some(s) = e.as_str() {
                let _ = write!(out, "\n  - {s}");
            }
        }
    }
    out.push_str(
        "\nNext: list_purchases to see the cached orders, or get_purchase(order_id) for one.",
    );
    out
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// First present string value among `keys`, or "—".
fn field_str<'a>(v: &'a Value, keys: &[&str]) -> &'a str {
    for k in keys {
        if let Some(s) = v.get(*k).and_then(Value::as_str) {
            return s;
        }
    }
    "—"
}

/// Render a price cell from whatever price field is present.
fn price_cell(v: &Value) -> String {
    if let Some(s) = v.get("formatted_price").and_then(Value::as_str) {
        return s.to_string();
    }
    if let Some(cents) = v
        .get("price_usd_cents")
        .or_else(|| v.get("total_usd_cents"))
        .and_then(Value::as_i64)
    {
        return fmt_usd_cents(cents);
    }
    if let Some(usd) = v
        .get("price_usd")
        .or_else(|| v.get("total_usd"))
        .and_then(Value::as_f64)
    {
        return format!("${}", trim_float(usd));
    }
    "—".to_string()
}

/// Render a credit-balance cell.
fn balance_cell(v: &Value) -> String {
    if let Some(s) = v.get("formatted_balance").and_then(Value::as_str) {
        return s.to_string();
    }
    if let Some(cents) = v.get("balance_cents").and_then(Value::as_i64) {
        return fmt_usd_cents(cents);
    }
    "—".to_string()
}

/// Money from a cents field (first key present), formatted as `$X.YZ`.
fn money_field(v: &Value, keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Some(c) = v.get(*k).and_then(Value::as_i64) {
            return Some(fmt_usd_cents(c));
        }
    }
    None
}

/// Integer cents → `$1,234.56`-ish (no thousands separator; kept simple).
fn fmt_usd_cents(cents: i64) -> String {
    let sign = if cents < 0 { "-" } else { "" };
    let abs = cents.abs();
    format!("{sign}${}.{:02}", abs / 100, abs % 100)
}

/// Trim a float to at most 6 decimals without trailing zeros.
fn trim_float(f: f64) -> String {
    let s = format!("{f:.6}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    s.to_string()
}

/// Shorten a hex hash to `0xabcd…wxyz`. Short strings pass through.
fn short_hash(h: &str) -> String {
    let body = h.strip_prefix("0x").unwrap_or(h);
    if body.len() <= 12 {
        return h.to_string();
    }
    format!("0x{}…{}", &body[..6], &body[body.len() - 4..])
}

/// Truncate a string to `max` chars, appending an elision marker.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    format!("{head}… ({} chars total)", s.chars().count())
}

/// Humanize a duration in seconds (e.g. 5 → "5s", 120 → "2m", 7200 → "2h").
fn human_duration(secs: i64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

/// Last-resort compact JSON (single line) for unexpected shapes.
fn compact(data: &Value) -> String {
    serde_json::to_string(data).unwrap_or_else(|_| "<unrenderable>".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn listings_lists_items_with_cursor_and_steer() {
        let data = json!({
            "data": [
                {"id": "L1", "title": "Demo", "seller_slug": "alice", "price_usd": 1.23, "delivery_eta_seconds": 8},
                {"id": "L2", "title": "Other", "price_usd_cents": 1400},
            ],
            "next_cursor": "abc123",
        });
        let out = render("list_marketplace", &data);
        assert!(out.contains("L1") && out.contains("Demo"), "{out}");
        assert!(out.contains("@alice"), "{out}");
        assert!(out.contains("$1.23") || out.contains("$1.230000"), "{out}");
        assert!(out.contains("$14.00"), "{out}");
        assert!(out.contains("~8s"), "{out}");
        assert!(out.contains("abc123"), "cursor echoed: {out}");
        assert!(out.contains("get_listing"), "steer present: {out}");
    }

    #[test]
    fn listings_empty_is_safe() {
        let out = render("list_marketplace", &json!({"data": []}));
        assert!(out.contains("No listings"), "{out}");
        assert!(!out.contains("1."), "no phantom item: {out}");
    }

    #[test]
    fn listing_with_schema_surfaces_required_fields() {
        let data = json!({"data": {
            "id": "L42", "title": "Run Python",
            "buyer_note_schema": {"type": "object", "required": ["code"], "properties": {"code": {"type": "string"}}}
        }});
        let out = render("get_listing", &data);
        assert!(out.contains("L42"), "{out}");
        assert!(out.contains("code"), "required field named: {out}");
        assert!(out.contains("create_order"), "{out}");
    }

    #[test]
    fn listing_without_schema_says_freeform() {
        let out = render("get_listing", &json!({"data": {"id": "L1", "title": "T"}}));
        assert!(
            out.contains("free-form") || out.contains("optional"),
            "{out}"
        );
    }

    #[test]
    fn orders_list_and_empty() {
        let data = json!({"data": [{"id": "O1", "payment_status": "paid", "fulfillment_status": "shipping"}]});
        let out = render("get_wallet_orders", &data);
        assert!(
            out.contains("O1") && out.contains("paid") && out.contains("shipping"),
            "{out}"
        );
        let empty = render("get_wallet_orders", &json!({"data": []}));
        assert!(empty.contains("No orders"), "{empty}");
    }

    #[test]
    fn order_pending_steers_to_pay() {
        let data = json!({"data": {"id": "O1", "payment_status": "pending", "fulfillment_status": "pending"}});
        let out = render("get_order_status", &data);
        assert!(out.to_lowercase().contains("pay"), "{out}");
    }

    #[test]
    fn order_paid_in_progress_steers_to_wait() {
        let data = json!({"data": {"id": "O1", "payment_status": "paid", "fulfillment_status": "awaiting_seller"}});
        let out = render("get_order_status", &data);
        assert!(out.contains("wait_for_order"), "{out}");
    }

    #[test]
    fn order_delivered_with_cached_pointer_steers_to_get_purchase() {
        let data = json!({"data": {
            "id": "O1", "payment_status": "paid", "fulfillment_status": "delivered",
            "delivered_content_cached": {"size_bytes": 3000, "hint": "x"}
        }});
        let out = render("get_order_status", &data);
        assert!(out.contains("3000"), "size shown: {out}");
        assert!(out.contains("get_purchase"), "{out}");
    }

    #[test]
    fn wait_for_order_reports_waited_and_timed_out() {
        let hit = json!({"data": {"id": "O1", "fulfillment_status": "delivered"}, "waited_seconds": 4, "timed_out": false});
        let out = render("wait_for_order", &hit);
        assert!(out.contains("Reached after 4s"), "{out}");
        let to = json!({"data": {"id": "O1", "fulfillment_status": "shipping"}, "waited_seconds": 60, "timed_out": true});
        let out2 = render("wait_for_order", &to);
        assert!(out2.contains("Timed out after 60s"), "{out2}");
    }

    #[test]
    fn credits_list_and_single() {
        let list = json!({"data": [{"seller_slug": "alice", "balance_cents": 1234, "formatted_balance": "$12.34"}]});
        let out = render("get_merchant_credits", &list);
        assert!(out.contains("@alice") && out.contains("$12.34"), "{out}");
        assert!(out.contains("redeem_merchant_credits"), "{out}");
        let single = json!({"seller_slug": "bob", "balance_cents": 5000});
        let out2 = render("get_merchant_credits", &single);
        assert!(out2.contains("@bob") && out2.contains("$50.00"), "{out2}");
    }

    #[test]
    fn redeem_shows_applied_and_remaining() {
        let data = json!({"status": "applied", "amount_redeemed_cents": 1500, "credit_balance_cents": 3500, "message": "Applied $15.00"});
        let out = render("redeem_merchant_credits", &data);
        assert!(out.contains("$15.00"), "applied: {out}");
        assert!(out.contains("$35.00"), "remaining: {out}");
    }

    #[test]
    fn buy_success_shows_tx_and_steer() {
        let data = json!({
            "order_id": "ord_buy_001", "tx_hash": "0x6d6b56b3acba7ebe1e44b8a5b1bb9d8a89e9ae37e9b97c4a4e0e0e0e0e0e0e0e",
            "payment_amount_usdc": 7.5, "status": "payment_sent", "note": "Credits will be funded automatically.",
            "order_url": "https://x/o"
        });
        let out = render("buy", &data);
        assert!(out.contains("ord_buy_001"), "{out}");
        assert!(out.contains("7.5 USDC"), "{out}");
        assert!(out.contains("0x6d6b56…0e0e"), "short hash: {out}");
        assert!(out.contains("wait_for_order"), "{out}");
    }

    #[test]
    fn buy_soft_error_shows_order_url() {
        let data = json!({
            "error": "Seller does not have a USDC wallet configured for direct payment.",
            "order_id": "ord_partial", "order_url": "https://example.com/orders/ord_partial",
            "hint": "Visit order_url to pay via web checkout."
        });
        let out = render("buy", &data);
        assert!(out.starts_with("⚠️"), "{out}");
        assert!(out.contains("ord_partial"), "{out}");
        assert!(
            out.contains("https://example.com/orders/ord_partial"),
            "{out}"
        );
        assert!(out.contains("web checkout"), "{out}");
    }

    #[test]
    fn send_renders_short_hash() {
        let out = render(
            "send_usdc",
            &json!({"tx_hash": "0x6d6b56b3acba7ebe1e44b8a5b1bb9d8a89e9ae37e9b97c4a4e0e0e0e0e0e0e0e"}),
        );
        assert!(out.contains("0x6d6b56…0e0e"), "{out}");
        assert!(out.contains("automatically"), "{out}");
    }

    #[test]
    fn purchases_list_and_empty() {
        let data = json!({"count": 1, "purchases": [{"order_id": "ord1", "title": "T", "fulfillment_status": "delivered"}]});
        let out = render("list_purchases", &data);
        assert!(out.contains("1 cached") && out.contains("ord1"), "{out}");
        let empty = render("list_purchases", &json!({"count": 0, "purchases": []}));
        assert!(empty.contains("No cached purchases"), "{empty}");
    }

    #[test]
    fn purchase_cached_and_not_cached() {
        let cached = json!({"order_id": "ord1", "title": "T", "fulfillment_status": "delivered", "delivered_content": "the answer is 42"});
        let out = render("get_purchase", &cached);
        assert!(out.contains("the answer is 42"), "{out}");
        let miss = json!({"error": "not_cached", "order_id": "nope"});
        let out2 = render("get_purchase", &miss);
        assert!(
            out2.contains("nope") && out2.contains("get_order_status"),
            "{out2}"
        );
    }

    #[test]
    fn sync_counts_and_errors() {
        let ok = render("sync_purchases", &json!({"synced": 2, "errors": []}));
        assert!(ok.contains("Synced 2"), "{ok}");
        let errs = render(
            "sync_purchases",
            &json!({"synced": 1, "errors": ["o9: boom"]}),
        );
        assert!(
            errs.contains("1 error") && errs.contains("o9: boom"),
            "{errs}"
        );
    }

    #[test]
    fn account_renders_markdown_rows() {
        let data = json!({
            "address": "0xabc", "network": "eip155:8453", "npub": "npub1alice",
            "eth_balance": {"formatted": "1", "symbol": "ETH"},
            "usdc_balance": {"formatted": "5", "symbol": "USDC"},
            "account": {"data": {"username": "alice", "formatted_account_number": "0001-0002"}}
        });
        let out = render("get_account_info", &data);
        assert!(out.starts_with("| Field | Value |"), "{out}");
        assert!(out.contains("| Network | eip155:8453 |"), "{out}");
        assert!(out.contains("| ETH Balance | 1 ETH (Base) |"), "{out}");
        assert!(out.contains("| USDC Balance | 5 USDC (Base) |"), "{out}");
        assert!(out.contains("| Username | alice |"), "{out}");
        assert!(out.contains("| Account Number | 0001-0002 |"), "{out}");
    }

    #[test]
    fn account_balance_error_falls_back() {
        let data = json!({"address": "0xabc", "network": "n", "npub": "npub1", "balance_error": "Could not fetch balance: boom"});
        let out = render("get_account_info", &data);
        assert!(out.contains("Could not fetch balance"), "{out}");
    }

    #[test]
    fn error_preserves_message_and_appends_hint() {
        let e = ToolError::NoWallet;
        let out = render_error(&e);
        assert!(out.contains("no wallet selected"), "base preserved: {out}");
        assert!(out.contains("owallet select"), "hint appended: {out}");

        let e2 = ToolError::NotAuthorized;
        assert!(render_error(&e2).contains("owallet authorize"));

        let e3 = ToolError::InvalidArg {
            arg: "amount_usd",
            reason: "must be a positive number".into(),
        };
        let out3 = render_error(&e3);
        assert!(out3.contains("positive"), "{out3}");

        let e4 = ToolError::WaitTimeout {
            target: "delivered".into(),
            seconds: 60,
        };
        assert!(render_error(&e4).contains("wait_for_order"));
    }

    #[test]
    fn helpers_behave() {
        assert_eq!(short_hash("0xabcdef0123456789"), "0xabcdef…6789");
        assert_eq!(short_hash("0xshort"), "0xshort");
        assert_eq!(fmt_usd_cents(1234), "$12.34");
        assert_eq!(fmt_usd_cents(5), "$0.05");
        assert_eq!(trim_float(7.5), "7.5");
        assert_eq!(trim_float(1.230000), "1.23");
        assert_eq!(human_duration(8), "8s");
        assert_eq!(human_duration(120), "2m");
        assert_eq!(human_duration(7200), "2h");
        assert!(render_soft_error("x", &json!({"error": "boom"}))
            .unwrap()
            .contains("boom"));
        assert!(render_soft_error("x", &json!({"ok": 1})).is_none());
    }
}
