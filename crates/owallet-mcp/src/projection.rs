//! Allowlist projections shared by the `/v1` chat surface and the MCP
//! transport (fathom-x/overpay#391).
//!
//! Everything a tool returns over an externally-reachable surface can end
//! up in a model's context and hence on a third party's servers — the
//! OpenRouter seller on `/v1`, Anthropic's on `/mcp`. The rule on both is
//! the same: **no on-chain data ever appears in a tool result** — no
//! txids, tx hashes, wallet addresses, npubs, pubkeys, or account
//! numbers. Order/listing ids, amounts, statuses, and spending limits
//! only. On-chain details remain on the surfaces the user reads directly:
//! the CLI and the dashboard in their own browser.
//!
//! Every function here is an *allowlist*: output is built from named
//! fields, never by copying whole payload objects — so a field added to a
//! handler (or to Rails) later cannot silently leak. When extending,
//! add fields by name and keep that property.
//!
//! [`sanitize`] is the MCP-surface entry point: it maps a tool name to
//! its projection while **preserving the tool's envelope shape**
//! (`{data: [...]}` etc.), so programmatic MCP clients keep their
//! contract and only the field vocabulary narrows. The `/v1` module
//! reuses the row-level builders and layers its spend-ledger context on
//! top.

use serde_json::{json, Map, Value};

/// Ceiling on how much `delivered_content` a projected `/v1` tool result
/// hands the model. See the doc on `openai_compat`'s use — everything in
/// `messages` re-ships to the OpenRouter seller every turn, so an
/// unbounded blob would blow the context for the rest of the chat. (The
/// MCP surface has its own cache-pointer stripping in `tools.rs` and does
/// not apply this cap.)
pub(crate) const DELIVERED_CONTENT_MODEL_CAP: usize = 8 * 1024;

// ---------------------------------------------------------------------------
// Shared row/section builders
// ---------------------------------------------------------------------------

/// One marketplace listing. Listings are public data, so this projection
/// is about consistency and context economy rather than secrecy:
/// image/checkout URLs and non-decision fields stay out. `detail` adds
/// the fields an ordering flow needs.
pub(crate) fn listing_row(listing: &Value, detail: bool) -> Value {
    let mut row = Map::new();
    if let Some(id) = listing.get("id").or_else(|| listing.get("listing_id")) {
        row.insert("listing_id".into(), id.clone());
    }
    for key in [
        "title",
        "description",
        "price_usd",
        "free",
        "currency",
        "category",
        "condition",
        "quantity",
        "delivery_eta_seconds",
    ] {
        if let Some(v) = listing.get(key) {
            row.insert(key.into(), v.clone());
        }
    }
    if let Some(slug) = listing.pointer("/seller/slug") {
        row.insert("seller_slug".into(), slug.clone());
    }
    if let Some(name) = listing.pointer("/seller/name") {
        row.insert("seller_name".into(), name.clone());
    }
    if detail {
        for key in ["buyer_note_schema", "delivered_content_type"] {
            if let Some(v) = listing.get(key).filter(|v| !v.is_null()) {
                row.insert(key.into(), v.clone());
            }
        }
    }
    Value::Object(row)
}

/// One compact order row for list output. The raw row carries
/// `settlement_tx_hash`, tracking fields, and the `buyer_note` — none of
/// that exists here.
pub(crate) fn order_summary_row(order: &Value) -> Value {
    let mut row = Map::new();
    if let Some(id) = order.get("id").or_else(|| order.get("order_id")) {
        row.insert("order_id".into(), id.clone());
    }
    for key in [
        "product_title",
        "payment_status",
        "fulfillment_status",
        "total_usd",
        "settled_amount_cents",
        "created_at",
    ] {
        if let Some(v) = order.get(key) {
            row.insert(key.into(), v.clone());
        }
    }
    Value::Object(row)
}

/// One full order for detail output (`create_order` / `get_order_status`
/// / `wait_for_order` on the MCP surface). Marketplace-side fields the
/// buyer acts on survive — statuses, amounts, tracking, the web order
/// URL, delivered content (already size-managed by `tools.rs`'s cache
/// stripping) — while `settlement_tx_hash`, `payment_address`, and
/// `buyer_note` do not.
pub(crate) fn order_detail(order: &Value) -> Value {
    let mut row = Map::new();
    if let Some(id) = order.get("id").or_else(|| order.get("order_id")) {
        row.insert("order_id".into(), id.clone());
    }
    for key in [
        "product_title",
        "payment_status",
        "fulfillment_status",
        "total_usd",
        "settled_amount_cents",
        "created_at",
        "paid_at",
        "delivered_at",
        "tracking_number",
        "tracking_carrier",
        "order_url",
        "delivered_content",
        "delivered_content_type",
        "delivered_content_url",
        "delivered_content_cached",
        "partial_content",
        "partial_seq",
        "error",
        "hint",
        "message",
        "status",
    ] {
        if let Some(v) = order.get(key) {
            row.insert(key.into(), v.clone());
        }
    }
    // The listing pointer lets a caller resolve the seller (pay_order does
    // this server-side too) — id + title only.
    if let Some(listing) = order.get("listing") {
        let mut l = Map::new();
        for key in ["id", "title"] {
            if let Some(v) = listing.get(key) {
                l.insert(key.into(), v.clone());
            }
        }
        if !l.is_empty() {
            row.insert("listing".into(), Value::Object(l));
        }
    }
    Value::Object(row)
}

/// Balances + merchant credits, as a map so callers can extend it
/// (the `/v1` surface adds its spend allowance / key budget).
pub(crate) fn balances_map(data: &Value) -> Map<String, Value> {
    let mut out = Map::new();
    if let Some(v) = data.pointer("/eth_balance/formatted") {
        out.insert("eth_balance".into(), v.clone());
    }
    if let Some(v) = data.pointer("/usdc_balance/formatted") {
        out.insert("usdc_balance".into(), v.clone());
    }
    if let Some(v) = data.pointer("/zec_balance/zec") {
        out.insert("zec_balance".into(), v.clone());
    }
    if let Some(v) = data.get("balance_error").and_then(Value::as_str) {
        // Free text from the EVM layer can embed the wallet's own address
        // ("invalid recipient address: 0x…"); an allowlist can't help
        // inside a string, so hex runs are scrubbed instead.
        out.insert("balance_error".into(), json!(scrub_hex(v)));
    }
    // Merchant credits arrive as the raw Rails `{data: [...]}` list; keep
    // only who holds them and how much.
    let credits = data
        .pointer("/merchant_credits/data")
        .or_else(|| data.get("merchant_credits"))
        .and_then(Value::as_array);
    if let Some(rows) = credits {
        let projected: Vec<Value> = rows
            .iter()
            .map(|c| {
                let mut row = Map::new();
                // `core` marks the overpay org's core-credit pool (the
                // marketplace's primary spend balance) — clients pin it first.
                for key in ["holder_type", "seller_slug", "organization_slug", "core"] {
                    if let Some(v) = c.get(key) {
                        row.insert(key.into(), v.clone());
                    }
                }
                if let Some(v) = c.get("balance_cents") {
                    row.insert("balance_cents".into(), v.clone());
                }
                Value::Object(row)
            })
            .collect();
        out.insert("merchant_credits".into(), json!(projected));
    }
    out
}

/// A credit redemption result (`pay_order`, and `redeem_merchant_credits`
/// once flattened), as a map so `/v1` can append its allowance fields.
pub(crate) fn pay_order_map(data: &Value) -> Map<String, Value> {
    let mut out = Map::new();
    for key in [
        "order_id",
        "seller_slug",
        "status",
        "amount_redeemed_cents",
        "credit_balance_cents",
        "remaining_balance_cents",
        "amount_due_cents",
        "message",
        "error",
        "hint",
    ] {
        if let Some(v) = data.get(key) {
            out.insert(key.into(), v.clone());
        }
    }
    out
}

// ---------------------------------------------------------------------------
// MCP-surface sanitization
// ---------------------------------------------------------------------------

/// Sanitize one MCP tool result for transmission off-machine. Envelope
/// shapes are preserved (`{data: ...}`, `{count, purchases}`, …) so
/// programmatic clients keep their contract; only the field vocabulary
/// narrows to the chain-free allowlist.
pub fn sanitize(tool: &str, data: &Value) -> Value {
    match tool {
        "get_account_info" => account_info(data),
        "list_marketplace" => {
            let rows: Vec<Value> = data
                .get("data")
                .and_then(Value::as_array)
                .map(|l| l.iter().map(|x| listing_row(x, false)).collect())
                .unwrap_or_default();
            envelope_list(rows, data)
        }
        "get_listing" => {
            json!({ "data": listing_row(data.get("data").unwrap_or(data), true) })
        }
        "get_wallet_orders" => {
            let rows: Vec<Value> = data
                .get("data")
                .and_then(Value::as_array)
                .map(|l| l.iter().map(order_summary_row).collect())
                .unwrap_or_default();
            envelope_list(rows, data)
        }
        "create_order" | "get_order_status" | "wait_for_order" => {
            let mut out = Map::new();
            out.insert(
                "data".into(),
                order_detail(data.get("data").unwrap_or(data)),
            );
            // wait_for_order splices these next to the envelope.
            for key in ["waited_seconds", "timed_out"] {
                if let Some(v) = data.get(key) {
                    out.insert(key.into(), v.clone());
                }
            }
            // A soft error may be flat (no envelope) — keep it readable.
            for key in ["error", "hint"] {
                if let Some(v) = data.get(key) {
                    out.insert(key.into(), v.clone());
                }
            }
            Value::Object(out)
        }
        "redeem_merchant_credits" => {
            // Raw passthrough is `{data: {...}}`; project the inner object
            // flat (matching pay_order's self-describing shape).
            Value::Object(pay_order_map(data.get("data").unwrap_or(data)))
        }
        "pay_order" => Value::Object(pay_order_map(data)),
        "buy" => buy_result(data),
        "send_usdc" | "send_zcash" => send_result(tool, data),
        "sync_zcash" => copy_fields(
            data,
            &["height", "balance_zec", "balance_zat", "spendable_zat"],
        ),
        "list_purchases" => {
            // Drops the top-level `npub` the raw shape carries.
            let rows: Vec<Value> = data
                .get("purchases")
                .and_then(Value::as_array)
                .map(|l| l.iter().map(purchase_row).collect())
                .unwrap_or_default();
            let mut out = Map::new();
            out.insert("count".into(), json!(rows.len()));
            out.insert("purchases".into(), json!(rows));
            if let Some(v) = data.get("error") {
                out.insert("error".into(), v.clone());
            }
            Value::Object(out)
        }
        "get_purchase" => purchase_row(data),
        "sync_purchases" => copy_fields(data, &["synced", "skipped", "errors", "error"]),
        // The bolt11 invoice IS the deliverable (the user must pay it);
        // only the redundant standalone payment_hash is dropped.
        "load_core_credits" => copy_fields(
            data,
            &[
                "order_id",
                "bolt11",
                "sats",
                "amount_cents",
                "expires_at",
                "order_url",
                "error",
                "hint",
            ],
        ),
        // The one-shot run_python purchase tool: the Python listing's
        // documented delivered shape, nothing else.
        "run_python" => copy_fields(
            data,
            &[
                "stdout",
                "stderr",
                "exit_code",
                "duration_ms",
                "timed_out",
                "error",
                "hint",
            ],
        ),
        // Any other name reaching here is a dynamic one-shot listing tool
        // (dispatch rejects truly unknown names before sanitization): keep
        // the `extract_listing_delivered` envelope — the deliverable and
        // its order id — plus the universally-safe keys the old catch-all
        // kept, and nothing a listing snapshot could smuggle beyond it.
        _ => copy_fields(
            data,
            &[
                "order_id",
                "fulfillment_status",
                "delivered_content",
                "delivered_content_truncated",
                "delivered_content_type",
                "delivered_content_url",
                "error",
                "hint",
                "status",
                "message",
            ],
        ),
    }
}

/// `{data: rows}` plus the cursor — the list envelope every Rails list
/// passthrough uses.
fn envelope_list(rows: Vec<Value>, raw: &Value) -> Value {
    let mut out = Map::new();
    out.insert("data".into(), json!(rows));
    if let Some(c) = raw.get("next_cursor").filter(|c| !c.is_null()) {
        out.insert("next_cursor".into(), c.clone());
    }
    if let Some(v) = raw.get("error") {
        out.insert("error".into(), v.clone());
    }
    Value::Object(out)
}

/// Replace every `0x`-prefixed hex run in free text with `0x…`. Used on
/// error strings that can quote an address; structured fields never need
/// this (they are allowlisted away instead).
fn scrub_hex(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find("0x") {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + 2..];
        let hex_len = after.bytes().take_while(|b| b.is_ascii_hexdigit()).count();
        if hex_len > 0 {
            out.push_str("0x…");
        } else {
            out.push_str("0x");
        }
        rest = &after[hex_len..];
    }
    out.push_str(rest);
    out
}

fn copy_fields(data: &Value, keys: &[&str]) -> Value {
    let mut out = Map::new();
    for key in keys {
        if let Some(v) = data.get(key) {
            out.insert((*key).into(), v.clone());
        }
    }
    Value::Object(out)
}

/// `get_account_info`: balances + credits + linkage status. The raw
/// handler output carries the EVM address, Schnorr pubkey, npub, Zcash
/// UA, and the Overpay account number — none of that exists here. The
/// `identity_note` tells the model (and the user reading the summary)
/// where those details actually live.
fn account_info(data: &Value) -> Value {
    let mut out = balances_map(data);
    for key in ["network", "chain_id", "account_hint", "error"] {
        if let Some(v) = data.get(key) {
            out.insert(key.into(), v.clone());
        }
    }
    // Keep the linked account's display name; drop the account number.
    if let Some(username) = data.pointer("/account/data/username") {
        out.insert("account".into(), json!({"data": {"username": username}}));
    }
    out.insert(
        "identity_note".into(),
        json!(
            "Wallet addresses and account identifiers are not included in tool output — \
             they are shown on the owallet dashboard (/wallet) and CLI."
        ),
    );
    Value::Object(out)
}

/// `buy`: the raw result carries `tx_hash`/`txid` and the payment
/// address; the caller gets the order id, status, amounts, and the web
/// order URL (the soft-error path's "pay via web checkout" fallback).
fn buy_result(data: &Value) -> Value {
    copy_fields(
        data,
        &[
            "order_id",
            "status",
            "payment_amount_usdc",
            "payment_amount_zec",
            "order_url",
            "note",
            "error",
            "hint",
        ],
    )
}

/// `send_usdc` / `send_zcash`: the transfer's success is the news; the
/// tx id stays on the operator surfaces. The recipient address transited
/// context as *input* (the user supplied it) — the output adds no
/// linkage.
fn send_result(tool: &str, data: &Value) -> Value {
    if data.get("error").is_some() {
        return copy_fields(data, &["error", "hint"]);
    }
    let asset = if tool == "send_zcash" { "ZEC" } else { "USDC" };
    json!({
        "status": "sent",
        "asset": asset,
        "note": "Transaction broadcast. The transaction id is viewable on the owallet dashboard and CLI.",
    })
}

/// One cached purchase (list row or `get_purchase` detail). The raw row
/// carries the wallet `npub` and the full `snapshot` (the unprojected
/// Rails order, settlement hash included) — neither survives. The cache
/// itself keeps full snapshots; only responses project.
fn purchase_row(data: &Value) -> Value {
    copy_fields(
        data,
        &[
            "order_id",
            "listing_id",
            "title",
            "seller",
            "payment_status",
            "fulfillment_status",
            "delivered_at",
            "paid_at",
            "total_usd_cents",
            "delivered_content",
            "delivered_content_type",
            "delivered_content_url",
            "delivered_content_schema",
            "schema_json",
            "cached_at",
            "error",
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every on-chain / identity field the raw handlers can produce.
    /// The single leak assertion below runs each tool's projection over a
    /// payload stuffed with all of them.
    const LEAK_MARKERS: &[&str] = &[
        "0xdeadbeef00000000000000000000000000000000",
        "0x6d6b56b3acba7ebe1e44b8a5b1bb9d8a",
        "txid_zc_secret",
        "npub1secret",
        "u1qqqsecretorchard",
        "02abcdefpubkey",
        "1234567890123456",
        "lnbc_payment_hash_secret",
    ];

    fn stuffed_order() -> Value {
        json!({
            "id": "ORD-1", "order_id": "ORD-1",
            "product_title": "Widget",
            "payment_status": "paid", "fulfillment_status": "delivered",
            "total_usd": "5.00", "created_at": "2026-08-12",
            "settlement_tx_hash": "0x6d6b56b3acba7ebe1e44b8a5b1bb9d8a",
            "payment_address": "0xdeadbeef00000000000000000000000000000000",
            "buyer_note": "{\"secret\": \"payload\"}",
            "delivered_content": "the goods",
            "listing": {"id": "L-1", "title": "Widget", "seller": {"slug": "acme", "npub": "npub1secret"}},
        })
    }

    fn assert_no_leaks(tool: &str, projected: &Value) {
        let text = projected.to_string();
        for marker in LEAK_MARKERS {
            assert!(
                !text.contains(marker),
                "{tool}: leaked '{marker}' in {text}"
            );
        }
    }

    #[test]
    fn every_tool_projection_drops_on_chain_and_identity_data() {
        let cases: Vec<(&str, Value)> = vec![
            (
                "get_account_info",
                json!({
                    "address": "0xdeadbeef00000000000000000000000000000000",
                    "pubkey": "02abcdefpubkey",
                    "npub": "npub1secret",
                    "zcash_address": "u1qqqsecretorchard",
                    "network": "eip155:8453", "chain_id": 8453,
                    "eth_balance": {"raw": 5, "formatted": "0.005", "symbol": "ETH"},
                    "usdc_balance": {"raw": 12000000, "formatted": "12.0", "symbol": "USDC"},
                    "zec_balance": {"zec": "0.25", "total_zat": 25000000},
                    "account": {"data": {"username": "alice",
                        "account_number": "1234567890123456",
                        "formatted_account_number": "1234 5678 9012 3456"}},
                    "merchant_credits": {"data": [{"seller_slug": "acme", "balance_cents": 500,
                        "holder_npub": "npub1secret"}]},
                }),
            ),
            (
                "get_wallet_orders",
                json!({"data": [stuffed_order()], "next_cursor": "abc"}),
            ),
            ("get_order_status", json!({"data": stuffed_order()})),
            ("create_order", json!({"data": stuffed_order()})),
            (
                "wait_for_order",
                json!({"data": stuffed_order(), "waited_seconds": 3, "timed_out": false}),
            ),
            (
                "buy",
                json!({
                    "order_id": "ORD-2",
                    "tx_hash": "0x6d6b56b3acba7ebe1e44b8a5b1bb9d8a",
                    "txid": "txid_zc_secret",
                    "payment_address": "0xdeadbeef00000000000000000000000000000000",
                    "payment_amount_usdc": 5.0, "order_url": "https://x/orders/ORD-2",
                    "status": "payment_sent", "note": "Credits will be funded automatically.",
                }),
            ),
            (
                "send_usdc",
                json!({"tx_hash": "0x6d6b56b3acba7ebe1e44b8a5b1bb9d8a"}),
            ),
            ("send_zcash", json!({"txid": "txid_zc_secret"})),
            (
                "pay_order",
                json!({"order_id": "ORD-3", "seller_slug": "acme", "status": "fully_paid",
                       "amount_redeemed_cents": 100, "credit_balance_cents": 400,
                       "settlement_tx_hash": "0x6d6b56b3acba7ebe1e44b8a5b1bb9d8a"}),
            ),
            (
                "redeem_merchant_credits",
                json!({"data": {"status": "fully_paid", "amount_redeemed_cents": 100,
                       "payer_address": "0xdeadbeef00000000000000000000000000000000"}}),
            ),
            (
                "list_purchases",
                json!({"npub": "npub1secret", "count": 1, "purchases": [
                    {"order_id": "ORD-4", "title": "Widget", "fulfillment_status": "delivered",
                     "npub": "npub1secret",
                     "snapshot": {"settlement_tx_hash": "0x6d6b56b3acba7ebe1e44b8a5b1bb9d8a"}}
                ]}),
            ),
            (
                "get_purchase",
                json!({"order_id": "ORD-4", "title": "Widget", "npub": "npub1secret",
                       "delivered_content": "the goods",
                       "snapshot": {"settlement_tx_hash": "0x6d6b56b3acba7ebe1e44b8a5b1bb9d8a",
                                    "payment_address": "0xdeadbeef00000000000000000000000000000000"}}),
            ),
            (
                "load_core_credits",
                json!({"order_id": "ORD-5", "bolt11": "lnbc1safe", "sats": 100,
                       "amount_cents": 500, "expires_at": "2026-08-12T00:00:00Z",
                       "payment_hash": "lnbc_payment_hash_secret", "order_url": "https://x/o/5"}),
            ),
            (
                "list_marketplace",
                json!({"data": [{"id": "L-1", "title": "Widget", "price_usd": "5.00",
                    "image_url": "https://x/img.png",
                    "seller": {"slug": "acme", "npub": "npub1secret"}}]}),
            ),
            (
                "get_listing",
                json!({"data": {"id": "L-1", "title": "Widget",
                    "buyer_note_schema": {"type": "object"},
                    "seller": {"slug": "acme", "npub": "npub1secret"}}}),
            ),
            (
                "sync_purchases",
                json!({"synced": 2, "skipped": 1, "errors": ["ORD-9: timeout"]}),
            ),
            (
                "sync_zcash",
                json!({"height": 2900000, "balance_zec": "0.25", "balance_zat": 25000000,
                       "spendable_zat": 25000000}),
            ),
        ];
        for (tool, raw) in cases {
            let projected = sanitize(tool, &raw);
            assert_no_leaks(tool, &projected);
        }
    }

    #[test]
    fn balance_error_free_text_is_scrubbed_of_hex_runs() {
        let account = sanitize(
            "get_account_info",
            &json!({"balance_error":
                "Could not fetch balance: invalid recipient address: 0xdeadbeef00. Check EVM_RPC_URL."}),
        );
        let err = account["balance_error"].as_str().unwrap();
        assert!(!err.contains("0xdeadbeef00"), "scrubbed: {err}");
        assert!(err.contains("0x…"), "placeholder kept: {err}");
        assert!(err.contains("Check EVM_RPC_URL"), "tail kept: {err}");

        assert_eq!(scrub_hex("no hex here"), "no hex here");
        assert_eq!(scrub_hex("bare 0x prefix"), "bare 0x prefix");
        assert_eq!(scrub_hex("a 0xAB1 b 0xcd2 c"), "a 0x… b 0x… c");
    }

    #[test]
    fn envelopes_and_useful_fields_survive() {
        let orders = sanitize(
            "get_wallet_orders",
            &json!({"data": [stuffed_order()], "next_cursor": "c1"}),
        );
        assert_eq!(orders["data"][0]["order_id"], "ORD-1");
        assert_eq!(orders["next_cursor"], "c1");

        let detail = sanitize("get_order_status", &json!({"data": stuffed_order()}));
        assert_eq!(detail["data"]["order_id"], "ORD-1");
        assert_eq!(detail["data"]["delivered_content"], "the goods");
        assert_eq!(detail["data"]["listing"]["id"], "L-1");
        assert!(detail["data"].get("buyer_note").is_none());

        let wait = sanitize(
            "wait_for_order",
            &json!({"data": stuffed_order(), "waited_seconds": 7, "timed_out": true}),
        );
        assert_eq!(wait["waited_seconds"], 7);
        assert_eq!(wait["timed_out"], true);

        let account = sanitize(
            "get_account_info",
            &json!({"eth_balance": {"formatted": "0.005"},
                    "account": {"data": {"username": "alice", "account_number": "1234567890123456"}},
                    "network": "eip155:8453"}),
        );
        assert_eq!(account["eth_balance"], "0.005");
        assert_eq!(account["account"]["data"]["username"], "alice");
        assert_eq!(account["network"], "eip155:8453");

        let send = sanitize("send_usdc", &json!({"tx_hash": "0xabc"}));
        assert_eq!(send["status"], "sent");
        assert_eq!(send["asset"], "USDC");

        let buy = sanitize(
            "buy",
            &json!({"error": "USDC transfer failed", "order_id": "O-1",
                    "order_url": "https://x/o/1", "hint": "Pay via order_url."}),
        );
        assert_eq!(buy["error"], "USDC transfer failed");
        assert_eq!(buy["order_url"], "https://x/o/1");

        let purchases = sanitize(
            "list_purchases",
            &json!({"npub": "npub1x", "count": 1,
                    "purchases": [{"order_id": "O-2", "title": "T", "snapshot": {}}]}),
        );
        assert_eq!(purchases["count"], 1);
        assert_eq!(purchases["purchases"][0]["order_id"], "O-2");
        assert!(purchases.get("npub").is_none());
    }
}
