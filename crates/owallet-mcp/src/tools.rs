//! Tool registry and per-tool handlers.
//!
//! Each tool is async, takes the shared `McpState` plus a JSON `arguments`
//! object, and returns a bare `serde_json::Value`. The tool names mirror
//! `wallet_mcp/server.py:1418-2118`; all are fully wired (including the
//! alloy-backed `send_usdc` / `buy`).
//!
//! Handlers stay pure data fetchers: [`dispatch`] renders each `Value`
//! into a concise, model-facing summary via [`crate::render`] and returns
//! both legs in a [`ToolOutput`] (rendered text → MCP `content`, raw data
//! → `structuredContent`). See fathom-x/overpay#295.

use std::time::Duration;

use owallet_overpay::models::{ListingFilters, OrderFilters};
use serde::Deserialize;
use serde_json::{json, Value};

use owallet_crypto::derive_from_stored_seed;
use owallet_crypto::evm::Address;

use crate::state::{McpState, OwnedAuth, ResolveAuthError};

/// One tool entry in the catalog returned by `tools/list`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("missing argument: {0}")]
    MissingArg(&'static str),
    #[error("invalid argument {arg}: {reason}")]
    InvalidArg { arg: &'static str, reason: String },
    #[error("no wallet selected — run `owallet select` or pass a wallet identifier")]
    NoWallet,
    #[error("not authorized — link the wallet to Overpay first (`owallet authorize`)")]
    NotAuthorized,
    #[error("overpay: {0}")]
    Overpay(#[from] owallet_overpay::OverpayError),
    #[error("evm: {0}")]
    Evm(#[from] owallet_evm::EvmError),
    #[error("zcash: {0}")]
    Zcash(#[from] owallet_zcash::ZcashError),
    #[error("not yet implemented in this build")]
    NotImplemented,
    #[error("wait_for_order: target {target} not reached within {seconds}s")]
    WaitTimeout { target: String, seconds: u64 },
    #[error("internal: {0}")]
    Internal(String),
}

impl From<ResolveAuthError> for ToolError {
    fn from(err: ResolveAuthError) -> Self {
        match err {
            ResolveAuthError::NoWallet => Self::NoWallet,
            ResolveAuthError::DbLocked => Self::Internal("db locked".into()),
            ResolveAuthError::Db(e) => Self::Internal(e.to_string()),
            ResolveAuthError::Hd(e) => Self::Internal(e.to_string()),
            ResolveAuthError::Internal(s) => Self::Internal(s),
        }
    }
}

/// The full catalog of tool specs returned by `tools/list`.
pub fn catalog() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "get_account_info",
            description:
                "Show the active wallet's EVM address, Nostr npub, Overpay account, and merchant credit balances (when linked).",
            input_schema: schema_object(json!({})),
        },
        ToolSpec {
            name: "list_marketplace",
            description: "Browse Overpay marketplace listings. Supports optional category / seller / cursor filters.",
            input_schema: schema_object(json!({
                "category":    {"type": "string"},
                "seller_slug": {"type": "string"},
                "cursor":      {"type": "string"},
                "limit":       {"type": "integer", "default": 20, "minimum": 1, "maximum": 100},
            })),
        },
        ToolSpec {
            name: "get_wallet_orders",
            description: "Fetch the active wallet's orders. Requires authorization.",
            input_schema: schema_object(json!({
                "status":             {"type": "string"},
                "fulfillment_status": {"type": "string"},
                "limit":              {"type": "integer", "default": 20, "minimum": 1, "maximum": 100},
                "cursor":             {"type": "string"},
            })),
        },
        ToolSpec {
            name: "get_listing",
            description: "Fetch a single marketplace listing including \
                          its buyer_note_schema and checkout_schema. Call \
                          this before create_order on a listing whose \
                          buyer_note_schema declares a structured shape, \
                          so the buyer_note can be constructed to match.",
            input_schema: schema_with_required(
                json!({"listing_id": {"type": "string"}}),
                &["listing_id"],
            ),
        },
        ToolSpec {
            name: "create_order",
            description: "Create a pending order for a marketplace listing. \
                          Requires authorization. `buyer_note` may be a \
                          string (free-form) or any JSON value (object / \
                          array / etc.) — when the listing declares a \
                          buyer_note_schema the tool pre-validates the \
                          note locally against it and rejects with the \
                          violations spelled out, so an invalid note is \
                          caught before order creation rather than at \
                          seller-bot fulfillment.",
            input_schema: schema_with_required(
                json!({
                    "listing_id": {"type": "string"},
                    "buyer_note": {
                        "description":
                            "Either a JSON string for free-form notes, \
                             or any JSON value matching the listing's \
                             buyer_note_schema. Call get_listing first \
                             to see the schema."
                    },
                }),
                &["listing_id"],
            ),
        },
        ToolSpec {
            name: "get_order_status",
            description: "Snapshot of a single order (status, fulfillment_status, tracking). Terminal orders are cached locally; large delivered_content is stripped to a pointer unless `include_delivered_content` is true — fetch it later with get_purchase.",
            input_schema: schema_with_required(
                json!({
                    "order_id":                  {"type": "string"},
                    "include_delivered_content": {"type": "boolean", "default": false},
                }),
                &["order_id"],
            ),
        },
        ToolSpec {
            name: "wait_for_order",
            description:
                "Poll until the order reaches `until_status` (default \"delivered\") or a terminal status (failed / cancelled), then return its final snapshot plus `waited_seconds` and `timed_out`. Caches terminal orders and strips large delivered_content unless `include_delivered_content` is true.",
            input_schema: schema_with_required(
                json!({
                    "order_id":                  {"type": "string"},
                    "until_status":              {"type": "string", "default": "delivered"},
                    "timeout_seconds":           {"type": "integer", "default": 60, "minimum": 1, "maximum": 600},
                    "poll_interval_seconds":     {"type": "integer", "default": 5,  "minimum": 1, "maximum": 60},
                    "include_delivered_content": {"type": "boolean", "default": false},
                }),
                &["order_id"],
            ),
        },
        ToolSpec {
            name: "redeem_merchant_credits",
            description: "Apply previously-purchased merchant credits to settle an order. Returns the amount redeemed and the remaining credit balance.",
            input_schema: schema_with_required(
                json!({
                    "seller_slug": {"type": "string"},
                    "order_id":    {"type": "string"},
                }),
                &["seller_slug", "order_id"],
            ),
        },
        ToolSpec {
            name: "buy",
            description: "One-shot purchase of merchant credits: opens a credit-purchase order with Overpay, then signs and broadcasts a USDC transfer to the returned payment address. Returns the order id, tx hash, and the USDC amount sent.",
            input_schema: schema_with_required(
                json!({
                    "seller_slug": {"type": "string"},
                    "amount_usd":  {"type": "number"},
                }),
                &["seller_slug", "amount_usd"],
            ),
        },
        ToolSpec {
            name: "send_usdc",
            description: "Sign and broadcast an ERC-20 USDC transfer on the configured EVM chain (default Base mainnet). Returns `{tx_hash}`.",
            input_schema: schema_with_required(
                json!({
                    "to_address":  {"type": "string"},
                    "amount_usdc": {"type": "number"},
                }),
                &["to_address", "amount_usdc"],
            ),
        },
        ToolSpec {
            name: "send_zcash",
            description: "Sync, then sign and broadcast a shielded Zcash (Orchard) payment to a Unified Address (u1…). Returns `{txid}`.",
            input_schema: schema_with_required(
                json!({
                    "to_address": {"type": "string"},
                    "amount_zec": {"type": "number"},
                }),
                &["to_address", "amount_zec"],
            ),
        },
        ToolSpec {
            name: "sync_zcash",
            description: "Sync the wallet's Zcash (Orchard) state from lightwalletd and return `{height, balance_zec, balance_zat, spendable_zat}`.",
            input_schema: schema_object(json!({})),
        },
        ToolSpec {
            name: "list_purchases",
            description: "List orders this wallet has cached locally from past purchases. The cache fills automatically when an order reaches a terminal fulfillment status. Call this before issuing a new `buy` to check whether a deliverable is already paid for. `delivered_content` is omitted — fetch it with get_purchase.",
            input_schema: schema_object(json!({
                "limit":              {"type": "integer", "default": 50},
                "fulfillment_status": {"type": "string"},
            })),
        },
        ToolSpec {
            name: "get_purchase",
            description: "Return the cached order payload (including delivered_content and the listing schema) for this wallet. Returns `{error: \"not_cached\"}` if the order isn't cached yet — call get_order_status or wait_for_order first to populate it.",
            input_schema: schema_with_required(
                json!({"order_id": {"type": "string"}}),
                &["order_id"],
            ),
        },
        ToolSpec {
            name: "sync_purchases",
            description: "Backfill the local purchase cache from Overpay: fetch delivered orders and re-fetch each one's detail (so delivered_content is included), upserting into the cache. Idempotent. Returns `{synced, errors}`.",
            input_schema: schema_object(json!({
                "api_key": {"type": "string"},
            })),
        },
        ToolSpec {
            name: "load_core_credits",
            description: "Create a Lightning invoice to load Overpay core credits. Returns a BOLT11 invoice with a scannable QR code. Pay from any Lightning wallet; credits are funded automatically once the invoice settles. Call wait_for_order(order_id, until_status=\"paid\") to confirm payment.",
            input_schema: schema_with_required(
                json!({
                    "amount_usd": {"type": "number", "description": "Amount to load in USD (must meet the site minimum)"},
                }),
                &["amount_usd"],
            ),
        },
    ]
}

/// Output shape from a tool handler (fathom-x/overpay#295).
///
/// Every tool yields both legs:
/// - `text` — a concise, model-readable summary with a `Next:` steer
///   (built by [`crate::render`]). This is what lands in the MCP
///   `content` blocks and therefore in the model's context window.
/// - `data` — the raw `serde_json::Value` the handler produced, kept in
///   `structuredContent` for programmatic clients (it does **not** count
///   against the model's context).
///
/// Handlers themselves still return a bare `Value`; [`dispatch`] renders
/// the text and pairs the two together, so per-handler code stays a pure
/// data fetch.
pub struct ToolOutput {
    pub text: String,
    pub data: Value,
}

/// Dispatch a `tools/call` to the right handler, then render its `Value`
/// into the model-facing summary. Returns both the rendered `text` and
/// the raw `data` for the transport to place in `content` /
/// `structuredContent` respectively.
pub async fn dispatch(state: &McpState, name: &str, args: Value) -> Result<ToolOutput, ToolError> {
    let data: Value = match name {
        "get_account_info" => get_account_info(state).await?,
        "list_marketplace" => list_marketplace(state, args).await?,
        "get_wallet_orders" => get_wallet_orders(state, args).await?,
        "get_listing" => get_listing(state, args).await?,
        "create_order" => create_order(state, args).await?,
        "get_order_status" => get_order_status(state, args).await?,
        "wait_for_order" => wait_for_order(state, args).await?,
        "redeem_merchant_credits" => redeem_merchant_credits(state, args).await?,
        "buy" => buy(state, args).await?,
        "send_usdc" => send_usdc(state, args).await?,
        "send_zcash" => send_zcash(state, args).await?,
        "sync_zcash" => sync_zcash(state, args).await?,
        "list_purchases" => list_purchases(state, args).await?,
        "get_purchase" => get_purchase(state, args).await?,
        "sync_purchases" => sync_purchases(state, args).await?,
        "load_core_credits" => load_core_credits(state, args).await?,
        other => {
            return Err(ToolError::InvalidArg {
                arg: "name",
                reason: format!("unknown tool '{other}'"),
            })
        }
    };
    let text = crate::render::render(name, &data);
    Ok(ToolOutput { text, data })
}

// ---------------------------------------------------------------------------
// Tool implementations
// ---------------------------------------------------------------------------

async fn get_account_info(state: &McpState) -> Result<Value, ToolError> {
    use owallet_overpay::OverpayError;

    let npub = state.resolve_npub().ok_or(ToolError::NoWallet)?;
    let wallets = {
        let db = state
            .db
            .lock()
            .map_err(|e| ToolError::Internal(format!("db mutex: {e}")))?;
        db.list_wallets()
            .map_err(|e| ToolError::Internal(e.to_string()))?
    };
    let wallet = wallets
        .iter()
        .find(|w| w.npub == npub)
        .ok_or(ToolError::NoWallet)?;
    let address = wallet.address.as_deref().unwrap_or("");

    // Derive the x-only Schnorr pubkey hex so the JSON dump matches
    // `wallet_mcp/server.py:1730` ("pubkey").
    let seed = {
        let db = state
            .db
            .lock()
            .map_err(|e| ToolError::Internal(format!("db mutex: {e}")))?;
        db.read_seed(&npub)
            .map_err(|e| ToolError::Internal(e.to_string()))?
            .ok_or(ToolError::NoWallet)?
    };
    let sk = derive_from_stored_seed(&seed).map_err(|e| ToolError::Internal(e.to_string()))?;
    let pubkey_hex = hex::encode(owallet_crypto::xonly_pubkey(&sk).serialize());

    // Chain metadata: chain_id from CAIP-2 even when the chain isn't in
    // our USDC table, so the field always renders.
    let chain_info = owallet_evm::chains::from_caip2(&state.evm_network).ok();
    let chain_id = chain_info.as_ref().map(|c| c.chain_id).or_else(|| {
        state
            .evm_network
            .strip_prefix("eip155:")?
            .parse::<u64>()
            .ok()
    });

    // Field order mirrors the Python tool's dict initialisation in
    // `server.py:1726-1732`.
    let mut result = serde_json::Map::new();
    result.insert("address".into(), json!(address));
    result.insert("network".into(), json!(state.evm_network.clone()));
    if let Some(id) = chain_id {
        result.insert("chain_id".into(), json!(id));
    }
    result.insert("pubkey".into(), json!(pubkey_hex));
    result.insert("npub".into(), json!(npub.clone()));

    // Best-effort on-chain balances — Python wraps both legs in one try
    // block (`server.py:1734-1744`), surfacing a single `balance_error`
    // string on any failure.
    let mut balance_error: Option<String> = None;
    if let Some(ci) = chain_info.as_ref() {
        match owallet_evm::eth_balance(&state.evm_rpc_url, address).await {
            Ok(raw) => {
                let formatted = owallet_evm::format_amount(raw, 18);
                let raw_u128: u128 = raw.try_into().unwrap_or(u128::MAX);
                result.insert(
                    "eth_balance".into(),
                    balance_value(raw_u128, formatted, "ETH"),
                );
                match owallet_evm::usdc_balance(&state.evm_rpc_url, ci, address).await {
                    Ok(raw) => {
                        let formatted = owallet_evm::format_amount(raw, ci.usdc_decimals);
                        let raw_u128: u128 = raw.try_into().unwrap_or(u128::MAX);
                        result.insert(
                            "usdc_balance".into(),
                            balance_value(raw_u128, formatted, "USDC"),
                        );
                    }
                    Err(e) => {
                        balance_error = Some(format!(
                            "Could not fetch balance: {e}. Check EVM_RPC_URL is accessible."
                        ));
                        // Match Python: drop the partial eth_balance if
                        // the wrap-up exception fires mid-block.
                        result.remove("eth_balance");
                    }
                }
            }
            Err(e) => {
                balance_error = Some(format!(
                    "Could not fetch balance: {e}. Check EVM_RPC_URL is accessible."
                ));
            }
        }
    }
    if let Some(msg) = balance_error.as_ref() {
        result.insert("balance_error".into(), json!(msg.clone()));
    }

    // Live Overpay fetch via the best available auth strategy: stored
    // bearer wins, otherwise NIP-98 fallback. Failures surface as an
    // `account_hint` string matching `server.py:1759-1766`.
    let account_attempt = match state.resolve_owned_auth() {
        Ok((_, owned)) => Some(state.overpay.account_value(owned.as_auth()).await),
        Err(_) => None,
    };
    match account_attempt {
        None => {
            result.insert(
                "account_hint".into(),
                json!("Run `owallet authorize` to link your Overpay account."),
            );
        }
        Some(Ok(acct)) => {
            result.insert("account".into(), acct);
        }
        Some(Err(OverpayError::HttpStatus { status, .. })) => {
            let hint = match status {
                401 | 403 => {
                    "Not authorized — run `owallet authorize` to link your Overpay account."
                        .to_string()
                }
                404 => format!(
                    "No Overpay account linked to this wallet. Sign up at {}",
                    state.overpay.base_url().as_str().trim_end_matches('/')
                ),
                _ => format!("Could not fetch account info: HTTP {status}"),
            };
            result.insert("account_hint".into(), json!(hint));
        }
        Some(Err(e)) => {
            result.insert(
                "account_hint".into(),
                json!(format!("Could not reach Overpay: {e}")),
            );
        }
    }

    // Best-effort merchant credits — same auth as account; skip silently on any failure.
    if let Ok((_, owned)) = state.resolve_owned_auth() {
        if let Ok(credits) = state
            .overpay
            .list_merchant_credits_value(owned.as_auth())
            .await
        {
            result.insert("merchant_credits".into(), credits);
        }
    }

    // Zcash receive address + balance. Auto-sync first (best-effort,
    // sync-on-read like zkv); the sync fast-path keeps repeat calls cheap, and
    // a failure just falls back to the last-known local balance.
    if let Some(ua) = wallet.zcash_address.as_deref() {
        result.insert("zcash_address".into(), json!(ua));
        if let (Ok(zseed), Ok(net), Ok(dir)) = (
            owallet_crypto::bip39_seed_from_stored(&seed),
            state.zcash_net(),
            state.zcash_data_dir(&npub),
        ) {
            let lwd = state.zcash_lightwalletd.clone();
            let sync_dir = dir.clone();
            // `zseed` is `[u8; 64]` (Copy), so it's still usable below.
            let _ = blocking_zcash(move |rt| {
                rt.block_on(async move {
                    owallet_zcash::init_account(&sync_dir, net, &lwd, &zseed, None).await?;
                    owallet_zcash::sync(&sync_dir, net, &lwd).await
                })
            })
            .await;
            if let Ok(bal) = owallet_zcash::zec_balance(&dir, net) {
                result.insert(
                    "zec_balance".into(),
                    json!({
                        "zec": owallet_zcash::format_zec(bal.total_zat),
                        "total_zat": bal.total_zat,
                        "spendable_zat": bal.spendable_zat,
                    }),
                );
            }
        }
    }

    // The markdown summary table is built by `render::render_account`
    // from this same object; the handler just returns the structured
    // data (which also becomes `structuredContent`).
    Ok(Value::Object(result))
}

/// Build a `{raw, formatted, symbol}` balance value matching the shape
/// in `server.py:1737,1742`. `raw` is emitted as a JSON number when it
/// fits in u64 (covers any realistic wallet balance) and as a string
/// otherwise so big-uint precision is preserved.
fn balance_value(raw_u128: u128, formatted: String, symbol: &str) -> Value {
    let raw_for_json = if raw_u128 <= u64::MAX as u128 {
        json!(raw_u128 as u64)
    } else {
        json!(raw_u128.to_string())
    };
    json!({
        "raw":       raw_for_json,
        "formatted": formatted,
        "symbol":    symbol,
    })
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct MarketplaceArgs {
    category: Option<String>,
    seller_slug: Option<String>,
    cursor: Option<String>,
    limit: Option<u32>,
}

async fn list_marketplace(state: &McpState, args: Value) -> Result<Value, ToolError> {
    let args: MarketplaceArgs =
        serde_json::from_value(args).map_err(|e| ToolError::InvalidArg {
            arg: "arguments",
            reason: e.to_string(),
        })?;
    // Keep the `{data: [...]}` envelope + nested `{seller: {...}}` shape, but
    // flatten each listing's `delivery_eta` object down to a scalar
    // `delivery_eta_seconds` (p50) — matches `list_marketplace` in
    // wallet_mcp/server.py.
    let mut body = state
        .overpay
        .list_listings_value(&ListingFilters {
            category: args.category,
            seller_slug: args.seller_slug,
            cursor: args.cursor,
            limit: args.limit.or(Some(20)),
        })
        .await?;
    if let Some(arr) = body.get_mut("data").and_then(Value::as_array_mut) {
        for listing in arr.iter_mut() {
            if let Some(obj) = listing.as_object_mut() {
                let p50 = obj
                    .remove("delivery_eta")
                    .as_ref()
                    .and_then(|e| e.get("p50_seconds"))
                    .cloned()
                    .unwrap_or(Value::Null);
                obj.insert("delivery_eta_seconds".into(), p50);
            }
        }
    }
    Ok(body)
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct OrderListArgs {
    /// Python's tool name. The catalog advertises `status`; Rails
    /// itself takes `payment_status` (which the tool body translates
    /// to). `payment_status` stays as an alias so existing Rust
    /// callers don't break.
    #[serde(alias = "payment_status")]
    status: Option<String>,
    fulfillment_status: Option<String>,
    limit: Option<u32>,
    cursor: Option<String>,
}

async fn get_wallet_orders(state: &McpState, args: Value) -> Result<Value, ToolError> {
    let args: OrderListArgs = serde_json::from_value(args).map_err(|e| ToolError::InvalidArg {
        arg: "arguments",
        reason: e.to_string(),
    })?;
    let (_npub, auth) = state.resolve_owned_auth()?;

    // Rails's NIP-98-authenticated orders endpoint requires a
    // `payer_address` query param so it can verify the signer matches
    // (orders_controller.rb#authorize_payer_address!). Bearer-authed
    // requests skip the check; passing the address anyway widens the
    // result set to "orders matching either the bearer's user OR the
    // address", which we don't want — only set it for NIP-98.
    let payer_address = match &auth {
        OwnedAuth::Nip98(sk) => Some(Address::from_private_key(sk).to_hex_lower()),
        OwnedAuth::Bearer(_) => None,
    };

    // Raw Rails passthrough — see list_marketplace and
    // fathom-x/overpay#288.
    state
        .overpay
        .list_orders_value(
            auth.as_auth(),
            &OrderFilters {
                payment_status: args.status,
                fulfillment_status: args.fulfillment_status,
                limit: args.limit.or(Some(20)),
                cursor: args.cursor,
                payer_address,
            },
        )
        .await
        .map_err(Into::into)
}

#[derive(Debug, Deserialize)]
struct CreateOrderArgs {
    listing_id: String,
    /// Buyer note. May be either a JSON string (free-form) or any JSON
    /// object/array — when the listing declares a `buyer_note_schema`
    /// the structured shape is required and the tool pre-validates it
    /// locally before submission.
    #[serde(default)]
    buyer_note: Option<Value>,
}

async fn create_order(state: &McpState, args: Value) -> Result<Value, ToolError> {
    let args: CreateOrderArgs =
        serde_json::from_value(args).map_err(|e| ToolError::InvalidArg {
            arg: "arguments",
            reason: e.to_string(),
        })?;
    let (_npub, auth) = state.resolve_owned_auth()?;

    // Best-effort pre-flight schema validation. Fetch the listing,
    // dig the `buyer_note_schema` out of the raw Rails JSON envelope,
    // and run `jsonschema` against the supplied note. Bot fulfillment
    // would otherwise JSON-parse the note and fail — locally catching
    // the violation lets the LLM correct and retry without burning a
    // Rails round-trip and a "Fulfillment failed" experience.
    //
    // Rails failures (network blip, 5xx) are non-blocking: we fall
    // through to submission rather than gating an order on a transient
    // outage. This is the only behaviour deliberately stricter than
    // Python's `server.py:1924` create_order (which is pure passthrough).
    if let (Some(note), Ok(listing_value)) = (
        args.buyer_note.as_ref(),
        state.overpay.get_listing_value(&args.listing_id).await,
    ) {
        // The listings#show endpoint wraps the payload in `{data: {...}}`
        // (Api::V1::BaseController#render_json, listings_controller.rb:35).
        let inner = listing_value.get("data").unwrap_or(&listing_value);
        if let Some(schema) = inner.get("buyer_note_schema") {
            // Treat empty or null schemas as "free-form" — match Rails's
            // `Listing#buyer_note_required?` predicate.
            let has_schema = matches!(schema, Value::Object(m) if !m.is_empty());
            if has_schema {
                // Buyer_note often arrives as a JSON-encoded string (Python's
                // `create_order` only accepts strings, and integration tests
                // / LLM clients send `json.dumps({"code": ...})`). Parse it
                // before validating, so a stringified object validates against
                // an object schema the same way a raw object would. If the
                // string doesn't parse or yields a string, validate the
                // original value (correctly rejects free-text against
                // non-string schemas).
                let parsed_string: Option<Value> = match note {
                    Value::String(s) => serde_json::from_str::<Value>(s)
                        .ok()
                        .filter(|v| !v.is_string()),
                    _ => None,
                };
                let for_validation = parsed_string.as_ref().unwrap_or(note);
                validate_buyer_note(schema, for_validation)?;
            }
        }
    }

    // Python wire parity: Rails receives `buyer_note` as a string
    // (`server.py:1924`'s `create_order` is typed `Optional[str]`; bot
    // fulfillment `JSON.parse`s it). If the caller handed us an object
    // or array, serialize it to a JSON string so the bot's JSON.parse
    // still works. Strings pass through verbatim; null/None stays null.
    let note_str: Option<String> = match args.buyer_note {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s),
        Some(v) => Some(serde_json::to_string(&v).map_err(|e| ToolError::Internal(e.to_string()))?),
    };

    state
        .overpay
        .create_order_value(&args.listing_id, note_str.as_deref(), auth.as_auth())
        .await
        .map_err(Into::into)
}

/// Validate `note` against the listing's `buyer_note_schema` using the
/// `jsonschema` crate. On failure, returns a [`ToolError::InvalidArg`]
/// whose `reason` enumerates each violation plus the schema title and
/// the schema JSON itself, so an LLM caller can correct and retry.
fn validate_buyer_note(schema: &Value, note: &Value) -> Result<(), ToolError> {
    let validator = match jsonschema::validator_for(schema) {
        Ok(v) => v,
        Err(e) => {
            // Malformed schema on the seller's side — surface as internal
            // error rather than rejecting the user's note for it.
            return Err(ToolError::Internal(format!(
                "listing.buyer_note_schema is not a valid JSON Schema: {e}"
            )));
        }
    };

    let errors: Vec<String> = validator
        .iter_errors(note)
        .map(|e| {
            let path = e.instance_path.to_string();
            if path.is_empty() {
                format!("{e}")
            } else {
                format!("{path}: {e}")
            }
        })
        .collect();
    if errors.is_empty() {
        return Ok(());
    }

    let title = schema
        .get("title")
        .and_then(|t| t.as_str())
        .unwrap_or("buyer_note");
    let schema_str = serde_json::to_string(schema).unwrap_or_else(|_| "<unrepresentable>".into());
    Err(ToolError::InvalidArg {
        arg: "buyer_note",
        reason: format!(
            "buyer_note does not match the listing's `{title}` schema:\n  - {}\n\nSchema: {schema_str}",
            errors.join("\n  - ")
        ),
    })
}

#[derive(Debug, Deserialize)]
struct ListingIdArg {
    listing_id: String,
}

/// Fetch a single marketplace listing including its
/// `buyer_note_schema`. Forwards the raw Rails response verbatim so
/// LLM callers can introspect the schema before calling `create_order`.
async fn get_listing(state: &McpState, args: Value) -> Result<Value, ToolError> {
    let args: ListingIdArg = serde_json::from_value(args).map_err(|e| ToolError::InvalidArg {
        arg: "arguments",
        reason: e.to_string(),
    })?;
    state
        .overpay
        .get_listing_value(&args.listing_id)
        .await
        .map_err(Into::into)
}

// ---------------------------------------------------------------------------
// Purchase cache + delivered-content stripping (server.py:2347-2420)
// ---------------------------------------------------------------------------

/// Fulfillment statuses that get mirrored into the local purchase cache.
const PURCHASE_CACHE_STATUSES: &[&str] = &[
    "delivered",
    "failed",
    "cancelled",
    "shipping",
    "awaiting_seller",
];

/// Above this many bytes, `delivered_content` is stripped from
/// get_order_status / wait_for_order responses (once cached) to keep agent
/// context windows small.
const DELIVERED_CONTENT_STRIP_THRESHOLD: usize = 2048;

/// Pull the order dict out of a Rails response envelope (handles `{data: …}`
/// and flat). Mirrors `_unwrap_order_payload`.
fn unwrap_order_payload(payload: &Value) -> Option<&Value> {
    if !payload.is_object() {
        return None;
    }
    if payload.get("data").map(Value::is_object).unwrap_or(false) {
        return payload.get("data");
    }
    if payload.get("order_id").is_some() || payload.get("id").is_some() {
        return Some(payload);
    }
    None
}

/// Best-effort: persist the order in the wallet's local cache if it's in a
/// cacheable fulfillment status, a wallet is active, and the DB is unlocked.
/// Returns whether a row was written. Silent on any failure. Mirrors
/// `_maybe_cache_purchase`.
fn maybe_cache_purchase(state: &McpState, payload: &Value) -> bool {
    let Some(order) = unwrap_order_payload(payload) else {
        return false;
    };
    let cacheable = order
        .get("fulfillment_status")
        .and_then(Value::as_str)
        .map(|s| PURCHASE_CACHE_STATUSES.contains(&s))
        .unwrap_or(false);
    if !cacheable {
        return false;
    }
    let Some(npub) = state.resolve_npub() else {
        return false;
    };
    let Ok(db) = state.db.lock() else {
        return false;
    };
    if !db.is_unlocked() {
        return false;
    }
    db.upsert_purchase(&npub, order).ok().flatten().is_some()
}

/// Drop bulky `delivered_content` from a response once it's been cached,
/// replacing it with a small `delivered_content_cached` pointer. Mutates
/// `payload` in place. Mirrors `_strip_large_delivered_content`.
fn strip_large_delivered_content(state: &McpState, payload: &mut Value) {
    // Locate the order object (envelope `data` or flat).
    let is_enveloped = payload.get("data").map(Value::is_object).unwrap_or(false);
    let is_flat = payload.get("order_id").is_some() || payload.get("id").is_some();
    let order = if is_enveloped {
        payload.get_mut("data")
    } else if is_flat {
        Some(&mut *payload)
    } else {
        None
    };
    let Some(order) = order else {
        return;
    };

    // Read what we need, then drop the immutable borrow before mutating.
    let size_bytes = match order.get("delivered_content").and_then(Value::as_str) {
        Some(c) if c.len() > DELIVERED_CONTENT_STRIP_THRESHOLD => c.len(),
        _ => return,
    };
    let content_type = order.get("delivered_content_type").cloned();
    let order_id = order
        .get("order_id")
        .or_else(|| order.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let (Some(npub), Some(order_id)) = (state.resolve_npub(), order_id) else {
        return;
    };
    // Only strip when we actually have a local cached copy to fall back on.
    {
        let Ok(db) = state.db.lock() else {
            return;
        };
        if db.read_purchase(&npub, &order_id).ok().flatten().is_none() {
            return;
        }
    }
    if let Some(obj) = order.as_object_mut() {
        obj.remove("delivered_content");
        obj.insert(
            "delivered_content_cached".into(),
            json!({
                "size_bytes": size_bytes,
                "content_type": content_type,
                "hint": format!(
                    "Call get_purchase(order_id='{order_id}') to retrieve the full content from the local cache."
                ),
            }),
        );
    }
}

#[derive(Debug, Deserialize)]
struct OrderIdArgs {
    order_id: String,
    /// Keep `delivered_content` inline even when it's been cached locally.
    /// Default false — large blobs are stripped to a pointer. Matches
    /// `get_order_status`'s arg in wallet_mcp/server.py.
    #[serde(default)]
    include_delivered_content: bool,
}

async fn get_order_status(state: &McpState, args: Value) -> Result<Value, ToolError> {
    let args: OrderIdArgs = serde_json::from_value(args).map_err(|e| ToolError::InvalidArg {
        arg: "arguments",
        reason: e.to_string(),
    })?;
    let (_npub, auth) = state.resolve_owned_auth()?;

    let mut data = state
        .overpay
        .get_order_value(&args.order_id, auth.as_auth())
        .await?;
    // Cache terminal orders, then strip the bulky delivered_content unless
    // the caller explicitly asked to keep it inline.
    maybe_cache_purchase(state, &data);
    if !args.include_delivered_content {
        strip_large_delivered_content(state, &mut data);
    }
    Ok(data)
}

#[derive(Debug, Deserialize)]
struct WaitForOrderArgs {
    order_id: String,
    /// Python's tool name; the older `target_fulfillment_status` is
    /// kept as an alias so existing Rust callers don't break.
    #[serde(default = "default_until_status", alias = "target_fulfillment_status")]
    until_status: String,
    #[serde(default = "default_timeout")]
    timeout_seconds: u64,
    #[serde(default = "default_poll")]
    poll_interval_seconds: u64,
    /// Keep `delivered_content` inline even once cached. Default false.
    #[serde(default)]
    include_delivered_content: bool,
}

fn default_until_status() -> String {
    "delivered".into()
}

/// Python `wait_for_order` defaults `timeout_seconds=60` (`server.py:1998`).
fn default_timeout() -> u64 {
    60
}

fn default_poll() -> u64 {
    5
}

/// Terminal statuses that short-circuit the polling loop alongside the
/// caller-supplied `until_status`. Matches `_WAIT_TERMINAL_STATUSES`
/// referenced by `server.py:2022`.
const WAIT_TERMINAL_STATUSES: &[&str] = &["failed", "cancelled"];

async fn wait_for_order(state: &McpState, args: Value) -> Result<Value, ToolError> {
    let args: WaitForOrderArgs =
        serde_json::from_value(args).map_err(|e| ToolError::InvalidArg {
            arg: "arguments",
            reason: e.to_string(),
        })?;
    // Cap the timeout matching `wait_for_order` in server.py:2020 (max
    // 600s) and clamp poll interval to [1, 60] like server.py:2021.
    let timeout = args.timeout_seconds.clamp(1, 600);
    let poll = args.poll_interval_seconds.clamp(1, 60);

    let (_npub, auth) = state.resolve_owned_auth()?;
    let start = std::time::Instant::now();
    loop {
        // Raw Rails passthrough — `snap` is `{"data": {...}}` so the
        // tool output shape matches Python (fathom-x/overpay#288).
        let snap = state
            .overpay
            .get_order_value(&args.order_id, auth.as_auth())
            .await?;
        let status = snap
            .get("data")
            .and_then(|d| d.get("fulfillment_status"))
            .or_else(|| snap.get("fulfillment_status"))
            .and_then(|v| v.as_str());

        let elapsed = start.elapsed().as_secs();
        let target_hit = status == Some(args.until_status.as_str());
        let terminal_hit = status
            .map(|s| WAIT_TERMINAL_STATUSES.contains(&s))
            .unwrap_or(false);

        if target_hit || terminal_hit {
            maybe_cache_purchase(state, &snap);
            let mut snap = snap;
            if !args.include_delivered_content {
                strip_large_delivered_content(state, &mut snap);
            }
            return Ok(splice_wait_meta(snap, elapsed, false));
        }
        if elapsed + poll >= timeout {
            maybe_cache_purchase(state, &snap);
            let mut snap = snap;
            if !args.include_delivered_content {
                strip_large_delivered_content(state, &mut snap);
            }
            return Ok(splice_wait_meta(snap, elapsed, true));
        }
        tokio::time::sleep(Duration::from_secs(poll)).await;
    }
}

/// Splice Python's `waited_seconds` + `timed_out` extra fields onto the
/// Rails snap, matching `server.py:2045-2047`. If `snap` is a JSON
/// object we mutate it in place; otherwise we wrap.
fn splice_wait_meta(mut snap: Value, waited_seconds: u64, timed_out: bool) -> Value {
    if let Some(obj) = snap.as_object_mut() {
        obj.insert("waited_seconds".into(), json!(waited_seconds));
        obj.insert("timed_out".into(), json!(timed_out));
        snap
    } else {
        json!({
            "snap": snap,
            "waited_seconds": waited_seconds,
            "timed_out": timed_out,
        })
    }
}

// ---------------------------------------------------------------------------
// Purchase cache tools (server.py:2626-2760)
// ---------------------------------------------------------------------------

/// Read the active wallet's npub, or return Python's `{"error": …}` dict
/// when there's no wallet / the DB is locked.
fn require_unlocked_npub(state: &McpState) -> std::result::Result<String, Value> {
    let Some(npub) = state.resolve_npub() else {
        return Err(json!({"error": "No wallet key configured."}));
    };
    let unlocked = state.db.lock().map(|db| db.is_unlocked()).unwrap_or(false);
    if !unlocked {
        return Err(json!({"error": "Wallet DB is locked. Unlock owallet first."}));
    }
    Ok(npub)
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct ListPurchasesArgs {
    limit: Option<i64>,
    fulfillment_status: Option<String>,
}

async fn list_purchases(state: &McpState, args: Value) -> Result<Value, ToolError> {
    let args: ListPurchasesArgs =
        serde_json::from_value(args).map_err(|e| ToolError::InvalidArg {
            arg: "arguments",
            reason: e.to_string(),
        })?;
    let npub = match require_unlocked_npub(state) {
        Ok(n) => n,
        Err(e) => return Ok(e),
    };
    let rows = {
        let db = state
            .db
            .lock()
            .map_err(|e| ToolError::Internal(format!("db mutex: {e}")))?;
        db.list_purchases(
            &npub,
            args.limit.unwrap_or(50),
            0,
            args.fulfillment_status.as_deref(),
        )
        .map_err(|e| ToolError::Internal(e.to_string()))?
    };
    // Strip the heavy fields from list rows — callers fetch them per order
    // via `get_purchase`.
    let summaries: Vec<Value> = rows
        .iter()
        .map(|r| {
            let mut v = serde_json::to_value(r).unwrap_or(Value::Null);
            if let Some(obj) = v.as_object_mut() {
                obj.remove("delivered_content");
                obj.remove("snapshot");
                obj.remove("delivered_content_schema");
            }
            v
        })
        .collect();
    Ok(json!({ "npub": npub, "count": summaries.len(), "purchases": summaries }))
}

#[derive(Debug, Deserialize)]
struct GetPurchaseArgs {
    order_id: String,
}

async fn get_purchase(state: &McpState, args: Value) -> Result<Value, ToolError> {
    let args: GetPurchaseArgs =
        serde_json::from_value(args).map_err(|e| ToolError::InvalidArg {
            arg: "arguments",
            reason: e.to_string(),
        })?;
    let npub = match require_unlocked_npub(state) {
        Ok(n) => n,
        Err(e) => return Ok(e),
    };
    let record = {
        let db = state
            .db
            .lock()
            .map_err(|e| ToolError::Internal(format!("db mutex: {e}")))?;
        db.read_purchase(&npub, &args.order_id)
            .map_err(|e| ToolError::Internal(e.to_string()))?
    };
    match record {
        Some(r) => serde_json::to_value(r).map_err(|e| ToolError::Internal(e.to_string())),
        None => Ok(json!({ "error": "not_cached", "order_id": args.order_id })),
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct SyncPurchasesArgs {
    #[allow(dead_code)]
    api_key: Option<String>,
}

async fn sync_purchases(state: &McpState, args: Value) -> Result<Value, ToolError> {
    let _args: SyncPurchasesArgs =
        serde_json::from_value(args).map_err(|e| ToolError::InvalidArg {
            arg: "arguments",
            reason: e.to_string(),
        })?;

    // Resolve npub + auth, mapping the no-wallet / locked / unauthorized
    // cases to Python's `{"error": …}` dicts.
    let (npub, auth) = match state.resolve_owned_auth() {
        Ok(x) => x,
        Err(ResolveAuthError::NoWallet) => {
            return Ok(json!({"error": "No wallet key configured."}))
        }
        Err(ResolveAuthError::DbLocked) => {
            return Ok(json!({"error": "Wallet DB is locked. Unlock owallet first."}))
        }
        Err(_) => return Ok(json!({"error": "Not authorized. Run `owallet authorize` first."})),
    };

    // NIP-98 requests must pin the payer_address; Bearer requests skip it.
    let payer_address = match &auth {
        OwnedAuth::Nip98(sk) => Some(Address::from_private_key(sk).to_hex_lower()),
        OwnedAuth::Bearer(_) => None,
    };

    let mut synced = 0u64;
    let mut errors: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut cursor: Option<String> = None;

    loop {
        let page = match state
            .overpay
            .list_orders_value(
                auth.as_auth(),
                &OrderFilters {
                    payment_status: None,
                    fulfillment_status: Some("delivered".into()),
                    limit: None,
                    cursor: cursor.clone(),
                    payer_address: payer_address.clone(),
                },
            )
            .await
        {
            Ok(p) => p,
            Err(e) => {
                errors.push(format!("list: {e}"));
                break;
            }
        };

        let Some(data) = page.get("data").and_then(Value::as_array) else {
            break;
        };
        if data.is_empty() {
            break;
        }

        for summary in data {
            let Some(oid) = summary
                .get("order_id")
                .or_else(|| summary.get("id"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            if !seen.insert(oid.to_string()) {
                continue;
            }
            match state.overpay.get_order_value(oid, auth.as_auth()).await {
                Ok(detail) => {
                    if let Some(order) = unwrap_order_payload(&detail) {
                        if let Ok(db) = state.db.lock() {
                            let _ = db.upsert_purchase(&npub, order);
                            synced += 1;
                        }
                    }
                }
                Err(e) => errors.push(format!("{oid}: {e}")),
            }
        }

        cursor = page
            .get("next_cursor")
            .and_then(Value::as_str)
            .map(str::to_string);
        if cursor.is_none() {
            break;
        }
    }

    Ok(json!({ "synced": synced, "errors": errors }))
}

// ---------------------------------------------------------------------------
// redeem_merchant_credits — apply stored credits to an existing order
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RedeemCreditsArgs {
    seller_slug: String,
    order_id: String,
}

async fn redeem_merchant_credits(state: &McpState, args: Value) -> Result<Value, ToolError> {
    let args: RedeemCreditsArgs =
        serde_json::from_value(args).map_err(|e| ToolError::InvalidArg {
            arg: "arguments",
            reason: e.to_string(),
        })?;
    let (_npub, auth) = state.resolve_owned_auth()?;
    // Raw Rails passthrough — see fathom-x/overpay#288.
    state
        .overpay
        .redeem_merchant_credits_value(&args.seller_slug, &args.order_id, auth.as_auth())
        .await
        .map_err(Into::into)
}

// ---------------------------------------------------------------------------
// buy — two-step compose: purchase credits order → on-chain USDC send
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct BuyArgs {
    seller_slug: String,
    amount_usd: f64,
}

async fn buy(state: &McpState, args: Value) -> Result<Value, ToolError> {
    let args: BuyArgs = serde_json::from_value(args).map_err(|e| ToolError::InvalidArg {
        arg: "arguments",
        reason: e.to_string(),
    })?;
    if !(args.amount_usd.is_finite() && args.amount_usd > 0.0) {
        return Err(ToolError::InvalidArg {
            arg: "amount_usd",
            reason: "must be a positive number".into(),
        });
    }
    // Round to whole cents — Rails endpoint takes integer cents.
    let amount_cents = (args.amount_usd * 100.0).round() as i64;

    // Step 1: open a credit-purchase order with Overpay. Auth required.
    let (npub, auth) = state.resolve_owned_auth()?;
    let purchase = state
        .overpay
        .purchase_merchant_credits(&args.seller_slug, amount_cents, auth.as_auth())
        .await?;

    // Zcash rail: if the server picked ZEC (Orchard UA + ZEC amount), pay
    // shielded. Detected via `PurchaseCreditsResponse::zcash_payment` (a
    // Zcash-shaped address, not an EVM `0x…`).
    if let Some((to_ua, amount_zec)) = purchase.zcash_payment() {
        let seed_str = {
            let db = state
                .db
                .lock()
                .map_err(|e| ToolError::Internal(format!("db mutex: {e}")))?;
            db.read_seed(&npub)
                .map_err(|e| ToolError::Internal(e.to_string()))?
                .ok_or(ToolError::NoWallet)?
        };
        let zseed = match owallet_crypto::bip39_seed_from_stored(&seed_str) {
            Ok(s) => s,
            Err(e) => {
                return Ok(json!({
                    "error":     format!("wallet has no Zcash account: {e}"),
                    "order_id":  purchase.order_id,
                    "order_url": purchase.order_url,
                    "hint":      "Pay via order_url, or import a mnemonic-backed wallet.",
                }));
            }
        };
        let network = state.zcash_net()?;
        let dir = state.zcash_data_dir(&npub)?;
        let lwd = state.zcash_lightwalletd.clone();
        let result = blocking_zcash(move |rt| {
            rt.block_on(async move {
                owallet_zcash::sync(&dir, network, &lwd).await?;
                owallet_zcash::send_zcash(&dir, network, &lwd, &zseed, &to_ua, amount_zec).await
            })
        })
        .await;
        return Ok(match result {
            Ok(send) => json!({
                "order_id":          purchase.order_id,
                "txid":              send.txid,
                "payment_amount_zec": amount_zec,
                "order_url":         purchase.order_url,
                "status":            "payment_sent",
                "note":              "Credits will be funded automatically once the transfer is detected on-chain.",
            }),
            Err(e) => json!({
                "error":     format!("ZEC transfer failed: {e}"),
                "order_id":  purchase.order_id,
                "order_url": purchase.order_url,
                "hint":      "Order created but payment not sent. Pay via order_url.",
            }),
        });
    }

    // Rails only fills payment_address + payment_amount_usdc when the
    // seller has a USDC wallet on file. For non-USDC sellers, return a
    // partial-success error dict matching `server.py:2154-2160` — the
    // order_id + order_url are still useful to the caller.
    let (Some(payment_address), Some(payment_amount_usdc)) = (
        purchase.payment_address.clone(),
        purchase.payment_amount_usdc,
    ) else {
        return Ok(json!({
            "error":     "Seller does not have a USDC wallet configured for direct payment.",
            "order_id":  purchase.order_id,
            "order_url": purchase.order_url,
            "hint":      "Visit order_url to pay via web checkout.",
        }));
    };

    // Step 2: on-chain USDC send to the address Rails just minted.
    let seed = {
        let db = state
            .db
            .lock()
            .map_err(|e| ToolError::Internal(format!("db mutex: {e}")))?;
        db.read_seed(&npub)
            .map_err(|e| ToolError::Internal(e.to_string()))?
            .ok_or(ToolError::NoWallet)?
    };
    let sk = derive_from_stored_seed(&seed).map_err(|e| ToolError::Internal(e.to_string()))?;
    let chain = owallet_evm::chains::from_caip2(&state.evm_network).map_err(ToolError::Evm)?;

    // If the on-chain send fails after the order was created, fall back
    // to a partial-success error dict (server.py:2162-2170) instead of
    // burning the order — the caller can still pay via the web URL.
    let send = match owallet_evm::send_usdc(
        &state.evm_rpc_url,
        &chain,
        &sk,
        &payment_address,
        payment_amount_usdc,
    )
    .await
    {
        Ok(s) => s,
        Err(e) => {
            return Ok(json!({
                "error":     format!("USDC transfer failed: {e}"),
                "order_id":  purchase.order_id,
                "order_url": purchase.order_url,
                "hint":      "Order created but payment not sent. Pay via order_url.",
            }));
        }
    };

    // Strict shape parity with Python `server.py:2172-2179`: only six
    // fields, in this order. Drops the richer `seller_slug` /
    // `payment_address` / `chain` / `block_number` / `explorer_url`
    // Rust used to emit.
    Ok(json!({
        "order_id":            purchase.order_id,
        "tx_hash":             send.tx_hash,
        "payment_amount_usdc": payment_amount_usdc,
        "order_url":           purchase.order_url,
        "status":              "payment_sent",
        "note":                "Credits will be funded automatically once the transfer is detected on-chain.",
    }))
}

// ---------------------------------------------------------------------------
// send_usdc — alloy-backed ERC-20 transfer on Base / any EVM chain
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct SendUsdcArgs {
    /// Python's name. `to` stays as an alias so existing Rust callers
    /// don't break.
    #[serde(alias = "to")]
    to_address: String,
    /// Python's name (`amount_usdc`); `amount` stays as an alias.
    #[serde(alias = "amount")]
    amount_usdc: f64,
}

async fn send_usdc(state: &McpState, args: Value) -> Result<Value, ToolError> {
    let args: SendUsdcArgs = serde_json::from_value(args).map_err(|e| ToolError::InvalidArg {
        arg: "arguments",
        reason: e.to_string(),
    })?;
    let npub = state.resolve_npub().ok_or(ToolError::NoWallet)?;
    let seed = {
        let db = state
            .db
            .lock()
            .map_err(|e| ToolError::Internal(format!("db mutex: {e}")))?;
        db.read_seed(&npub)
            .map_err(|e| ToolError::Internal(e.to_string()))?
            .ok_or(ToolError::NoWallet)?
    };
    let sk = derive_from_stored_seed(&seed).map_err(|e| ToolError::Internal(e.to_string()))?;
    let chain = owallet_evm::chains::from_caip2(&state.evm_network).map_err(ToolError::Evm)?;

    let outcome = owallet_evm::send_usdc(
        &state.evm_rpc_url,
        &chain,
        &sk,
        &args.to_address,
        args.amount_usdc,
    )
    .await?;

    // Strict parity with Python `server.py:1824-1825`: only `tx_hash`.
    Ok(json!({ "tx_hash": outcome.tx_hash }))
}

#[derive(Deserialize)]
struct SendZcashArgs {
    #[serde(alias = "to")]
    to_address: String,
    #[serde(alias = "amount")]
    amount_zec: f64,
}

/// Resolve the active wallet's BIP-39 seed, Zcash network, and per-wallet data
/// directory for the Zcash tools.
fn zcash_ctx(
    state: &McpState,
) -> Result<(String, [u8; 64], owallet_zcash::Network, std::path::PathBuf), ToolError> {
    let npub = state.resolve_npub().ok_or(ToolError::NoWallet)?;
    let seed_str = {
        let db = state
            .db
            .lock()
            .map_err(|e| ToolError::Internal(format!("db mutex: {e}")))?;
        db.read_seed(&npub)
            .map_err(|e| ToolError::Internal(e.to_string()))?
            .ok_or(ToolError::NoWallet)?
    };
    let seed =
        owallet_crypto::bip39_seed_from_stored(&seed_str).map_err(|e| ToolError::InvalidArg {
            arg: "wallet",
            reason: format!("wallet has no Zcash account: {e}"),
        })?;
    let network = state.zcash_net()?;
    let dir = state.zcash_data_dir(&npub)?;
    Ok((npub, seed, network, dir))
}

/// Drive an `owallet_zcash` async operation to completion on a dedicated
/// blocking thread with its own current-thread runtime.
///
/// librustzcash holds non-`Send` state (the rusqlite-backed `WalletDb`, the
/// gRPC client, the local prover) across await points, so its futures can't be
/// awaited directly inside an axum (`Send`-future) handler. Running them under
/// `spawn_blocking` keeps the whole non-`Send` future on one thread; the
/// handler only awaits the `Send` `JoinHandle`.
async fn blocking_zcash<T, F>(f: F) -> Result<T, ToolError>
where
    F: FnOnce(&tokio::runtime::Runtime) -> Result<T, owallet_zcash::ZcashError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(owallet_zcash::ZcashError::from)?;
        f(&rt)
    })
    .await
    .map_err(|e| ToolError::Internal(format!("zcash task: {e}")))?
    .map_err(ToolError::Zcash)
}

async fn send_zcash(state: &McpState, args: Value) -> Result<Value, ToolError> {
    let args: SendZcashArgs = serde_json::from_value(args).map_err(|e| ToolError::InvalidArg {
        arg: "arguments",
        reason: e.to_string(),
    })?;
    let (_npub, seed, network, dir) = zcash_ctx(state)?;
    let lwd = state.zcash_lightwalletd.clone();
    let to = args.to_address;
    let amount = args.amount_zec;
    // Sync first so the wallet has spendable notes, then broadcast.
    let outcome = blocking_zcash(move |rt| {
        rt.block_on(async move {
            owallet_zcash::sync(&dir, network, &lwd).await?;
            owallet_zcash::send_zcash(&dir, network, &lwd, &seed, &to, amount).await
        })
    })
    .await?;
    Ok(json!({ "txid": outcome.txid }))
}

async fn sync_zcash(state: &McpState, _args: Value) -> Result<Value, ToolError> {
    let (_npub, seed, network, dir) = zcash_ctx(state)?;
    let lwd = state.zcash_lightwalletd.clone();
    let balance = blocking_zcash(move |rt| {
        rt.block_on(async move {
            owallet_zcash::init_account(&dir, network, &lwd, &seed, None).await?;
            let height = owallet_zcash::sync(&dir, network, &lwd).await?;
            let balance = owallet_zcash::zec_balance(&dir, network)?;
            Ok::<_, owallet_zcash::ZcashError>((height, balance))
        })
    })
    .await?;
    let (height, balance) = balance;
    Ok(json!({
        "height": height,
        "balance_zec": owallet_zcash::format_zec(balance.total_zat),
        "balance_zat": balance.total_zat,
        "spendable_zat": balance.spendable_zat,
    }))
}

// ---------------------------------------------------------------------------
// load_core_credits — Lightning invoice for Overpay core credits
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct LoadCoreCreditsArgs {
    amount_usd: f64,
}

async fn load_core_credits(state: &McpState, args: Value) -> Result<Value, ToolError> {
    let args: LoadCoreCreditsArgs =
        serde_json::from_value(args).map_err(|e| ToolError::InvalidArg {
            arg: "arguments",
            reason: e.to_string(),
        })?;
    if !(args.amount_usd.is_finite() && args.amount_usd > 0.0) {
        return Err(ToolError::InvalidArg {
            arg: "amount_usd",
            reason: "must be a positive number".into(),
        });
    }
    let amount_cents = (args.amount_usd * 100.0).round() as i64;
    let (_, auth) = state.resolve_owned_auth()?;
    let resp = state
        .overpay
        .load_core_credits(amount_cents, auth.as_auth())
        .await?;
    Ok(json!({
        "order_id":     resp.order_id,
        "bolt11":       resp.bolt11,
        "payment_hash": resp.payment_hash,
        "sats":         resp.sats,
        "amount_cents": resp.amount_cents,
        "expires_at":   resp.expires_at,
        "order_url":    resp.order_url,
    }))
}

// ---------------------------------------------------------------------------
// JSON-schema helpers
// ---------------------------------------------------------------------------

fn schema_object(properties: Value) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "additionalProperties": false,
    })
}

fn schema_with_required(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}
