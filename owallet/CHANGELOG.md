# Changelog

All notable changes to the Rust port of `owallet` are documented here.

## Unreleased

### Orders are filed under the conversation that placed them

- Every order owallet places now carries a `client_session_id`: an HMAC of
  the caller's conversation id under the per-install secret (the same
  derivation as the OpenRouter `session_id`), never the id itself. `/v1`
  takes the conversation from `x-order-session-id`, else the request's
  conversation key; MCP tool calls take it from `_meta`
  (`overpay.com/session-id`), because one MCP connection serves every
  conversation in a harness. It sits beside the buyer note, not in it, so
  it applies to every listing and the seller never sees it.
- `get_wallet_orders` gains `scope`: `"session"` (the default) lists only
  the calling conversation's orders and ends with what the whole
  conversation has cost, read across up to 200 orders; `"all"` lists the
  wallet's. The model passes no id. When the caller named no conversation,
  or the marketplace predates the field and ignored the filter, every order
  is listed and the first line says these are all of the wallet's orders,
  not one conversation's.

### `get_wallet_orders` answers "what did these cost"

- Each row now shows when the order was placed (`2026-10-05 17:55Z`), and
  the list ends with the total charged for the paid orders listed, beside
  the total authorized when the two differ. The closing hint and the tool
  description say the per-row charge is the order's cost. A model asked
  what a conversation had spent used to open every order with
  `get_order_status` to read the charge and tell the turns apart.

### Every request signed by the wallet key

- With a stored Overpay token, owallet now also signs every request with
  the wallet key (the npub's — the same key as the wallet's EVM address)
  in an `X-Nostr-Signature` header: NIP-98 (kind 27235, `u` + `method`),
  plus for POSTs a `payload` tag — the sha256 of the exact body sent — so
  the signature covers what is bought, not just which URL was called.
  Overpay verifies it on every spend (orders including credit-paid model
  turns, merchant-credit redeem / purchase / load), stores it with the
  order, and rejects a replayed signature. NIP-98-only requests (no token)
  carry the same payload tag in `Authorization: Nostr`.
- `owallet credits load` signs its request too.

### Faster streaming in `/v1`

- Orders in flight are polled every 400 ms instead of every second, and the
  interval now runs start-to-start: it used to be a 1 s sleep *after* each
  request, so with a ~0.6 s request a streamed reply arrived in bursts
  every ~1.6 s. `OWALLET_V1_POLL_MS` overrides the interval.
- Polls are conditional on the last `partial_seq` seen (`?since_seq=`):
  the marketplace leaves an unchanged streaming buffer out of the
  response, so a long reply isn't re-downloaded several times a second.

### Plain completions for housekeeping calls

- **`x-owallet-tools: none`**: a `/v1` request that brings no tools of its
  own normally runs owallet's server-side loop with its whole roster
  (wallet tools, `run_python`, listing tools) attached. With this header
  it is one passthrough turn with no tools at all — no roster tokens in
  the prompt and no way for the model to buy something. For a client's
  titles, summaries and compaction (norm sends it for those). A request
  with its own tools is unchanged; any other header value is a 400.

### What each model costs, in `GET /v1/models`

- Each model entry now carries what its listing variant publishes, as
  optional OpenAI-compatible extensions: `name`, `context_length`,
  `active` (false once the seller stops offering it) and `pricing` —
  USD per million tokens with the seller's markup applied (`input`,
  `output`, `cache_read`, `request`, `long_context` tiers), the per-turn
  `min_charge`, the model's `min_authorization` (minimum commitment),
  `basis: "list"` and the catalog's `as_of`. List prices
  are the priciest provider the seller admitted at its last catalog
  fetch: an estimate's upper end, not a quote. `default` stays first and
  unpriced (the seller picks what it resolves to); a variant without a
  rate card is listed bare rather than at $0. Clients that read only `id`
  see no change.

### A per-message spending limit, and errors that say what to do

- **`x-owallet-request-max-usd`**: the most any single order of the
  request may authorize or cost (norm's `/budget` → "Per-message limit",
  default $1). Each OpenRouter turn's authorization is sized within it — a
  tight limit authorizes exactly the limit when the turn still fits — and
  a turn that can't fit (its input plus the seller's minimal reply, or the
  model's advertised minimum commitment) is refused **before any order is
  placed**: "This message (about 40,013 tokens of context on X) needs at
  least $0.15 — over your $0.10 per-message limit. Nothing was sent or
  charged. Raise the limit (norm: /budget), start a fresh conversation, or
  pick a cheaper model." A fixed-price tool purchase over the limit is
  refused before it is paid.
- The conversation budget (`x-owallet-spend-limit-usd`) now bounds turn
  authorizations too, not only the wallet tools; the lower of the two
  limits applies, and the refusal names which.
- **Error codes.** `error.code` (was always null) is now
  `request_limit_exceeded`, `authorization_too_low`, `budget_exhausted`,
  `insufficient_credits` or `model_unavailable`, so clients can react
  without parsing messages.
- **Seller rejections read as such.** An `authorization_too_low`
  rejection (fathom-x/overpay#473) becomes a limit error with the seller's
  "needs at least $X" message plus how to proceed; `upstream_unavailable`
  becomes `model_unavailable` with the seller's message ("No OpenRouter
  provider can serve X within this request's authorization… Nothing was
  charged"). Older sellers' error deliveries carrying the same
  `reason_code` map the same way.

### Keeping OpenRouter's prompt cache warm through `/v1`

Synced from fathom-x/overpay#456 (the owallet side of overpay#445's notes).

- **Cache counters in usage.** `usage.prompt_tokens_details` carries
  OpenRouter's `cached_tokens` / `cache_write_tokens`, summed across a
  request's orders — and only counts some order actually reported: absent
  means unknown (the listing's rebuilt-from-generation usage has none), not
  zero. `cached_tokens` is OpenAI's own field, so OpenAI-compatible clients
  (the AI SDK, so norm) show cache reads with no change.
- **Client cache markers survive.** `content` is no longer flattened: text
  parts pass through with their `cache_control`, and a message-level
  `cache_control` (how OpenAI-compatible clients mark single-text messages)
  becomes a marked one-part array. At most 3 client markers are forwarded,
  keeping the earliest — Anthropic allows 4 and the listing adds one on the
  newest message. Text parts are no longer glued together with no
  separator.
- **`session_id` for sticky routing**, behind `OWALLET_V1_SESSION_ID=1`.
  The client's conversation key — body `session_id`, then `x-session-id`,
  then `prompt_cache_key` / `promptCacheKey` — becomes
  `HMAC-SHA256(per-install secret, key)`, sent on every OpenRouter buyer
  note of the request; never the raw key or a per-wallet constant. Without
  a key: a random id for the server-side loop, nothing for passthrough.
  `run_python` and listing-tool orders never carry it.
- **Stable tool order.** Listing tools are sorted by name, so the tool
  block that opens every prompt can't reorder between requests.

Fixes from a live marketplace test session.

### Long chat replies no longer fail after being paid for

- Rails moves any `delivered_content` over 4 KB to object storage and
  returns `delivered_content_url` in its place. `/v1` read only the inline
  field, so every turn whose reply passed ~4 KB failed with `[owallet error]
  order has no delivered_content` — already charged, reply lost. Every
  `/v1` order poll — including one-shot tool executions — now goes through
  `get_order_resolved`, which downloads the signed file back inline (no
  credentials, marketplace origin only). This also restores the metered
  `charged_cents` settlement for those turns, and a failed order reports
  the seller's reason instead of the missing deliverable.

### One-shot purchases never lose a paid order

- A one-shot tool (`run_python` / `provider_tool` listings) whose seller
  stalls now returns the paid order as a pending result — `order_id`,
  statuses, `charged_cents`, "do not buy again", and a `wait_for_order`
  hint — instead of an error without the order id.
- On `/mcp`, in-flight polls emit `notifications/progress`, which resets
  the MCP client's request timeout (opencode's 60s tripped on a silent
  120s poll and dropped the result). A client that didn't opt into
  progress gets the pending result at 50s, before its own timeout.
- One-shot results render readably: JSON deliverables decoded instead of
  double-encoded, `run_python` as exit code + stdout/stderr, followed by
  an `order_id: …` line and what it charged.
- Image (and other binary) deliveries succeed: a paid `generate_image`
  failed with `delivered content: file is not UTF-8 text` although the
  seller had delivered a valid PNG. A file delivery now comes back as
  `delivered_content_url` + `delivered_content_type` +
  `delivered_content_bytes` ("Delivered a file (image/png, 1.0 MB)" and
  the download link); binary media types aren't downloaded at all.
- A one-shot order that was paid but whose result then couldn't be read
  returns its `order_id`, the charge, and "do not buy again" rather than
  a bare error.

### Compatible with Overpay's metered pricing (overpay#458-#466)

- **Long OpenRouter turns are authorized for what they need.** The
  listing's price is now a per-turn *authorization*, and the seller's
  exposure guard refuses a turn whose authorization can't cover its input
  plus output ("needs an authorization of at least N¢") — which a long
  agent conversation reaches on any model, sooner on premium ones. Every
  OpenRouter turn now sizes itself from the model variant's published
  rate card (the guard's own arithmetic: input at its tier, an 8192-token
  output allowance or the request's smaller `max_tokens`, plus markup)
  and, when that exceeds the default, creates and pays the order in one
  request with `authorization_cents`. The unused hold comes back as
  credits as before. If credits can't cover the larger hold the turn is
  placed the old way, so a low balance never loses a turn that fits.
- **`rejected` is a terminal order state.** A seller's refusal of a paid
  order (credits released in full) used to be polled until the timeout;
  it now ends the wait, and the error/next step carries the `rejection`
  reason and, for `authorization_too_low`, the authorization that would
  have been enough. Order projections keep `rejection` (field by field).
- **Status filters are Rails's to validate** (overpay#466 answers an
  unknown value with a 422 naming the allowed ones), so the order tools no
  longer carry a hardcoded copy of the enums — that copy was already
  missing `rejected`.
- **Free orders pay through `pay_order`** again: Rails now settles a $0.00
  order via the normal redemption, so the client-side "free order"
  short-circuit is gone.
- File deliveries take their size from Rails's
  `delivered_content_byte_size` when the download doesn't state one.

### Prices, amounts, and schemas visible to the model

- `list_marketplace` / `get_listing` showed `Price: —` for every listing:
  Rails sends `price_usd` as a formatted string, which the renderer
  ignored. Prices render from `price_usd` or `price_cents` (sub-cent aware).
- Orders show their amount (and a metered order's settled charge) and
  product title; projections keep `price_cents` / `total_usd_cents`.
- `get_listing` renders the buyer_note_schema's fields (type, required,
  enum, default, description) in the text — it was only in
  `structuredContent`, which the model never reads.
- Listings sold through a one-shot tool are marked with its name.

## 0.1.10

### `overpay_connected` in `GET /v1/status`

- The status payload now says whether the wallet is linked to an Overpay
  account (`account` fetch succeeded vs the authorize/sign-up hint), so
  norm's sidebar can tell the user to log in to Overpay to get started.

## 0.1.9

### Core credits marked in status and balances

- Merchant-credit rows in `get_account_info` / `GET /v1/status` now carry
  the Rails-provided `core` flag on the overpay org's core-credit pool,
  so clients (norm's sidebar) can pin the marketplace's primary spend
  balance first. Requires an Overpay deploy that tags the row; absent
  the flag nothing changes.

## 0.1.8

### Lenient ambient `.env` loading

- The `.env` picked up from the current working directory belongs to
  whatever project the user is standing in, and one line owallet could
  not parse used to fail the whole command — it killed `owallet init`
  during norm's first-run wallet setup. Unparsable lines are now skipped
  with a stderr note; explicit `--config` and discovered `*.owallet`
  files stay strict.

## 0.1.7

### Cost markers on every model-facing tool description (fathom-x/norm#17)

- Listing-backed tools (`run_python`, every `provider_tool` listing, on
  `/v1` and `/mcp` alike) now end their descriptions with a cost note —
  "Each call places a real marketplace order billed to the wallet
  (≈ $X per call)" — priced live from the listing's own `price_usd` /
  `free` fields, so a repriced listing needs no code change.
- The free wallet/order/marketplace reads on both rosters now open with
  "Free — a read, no order is placed and nothing is billed", and the
  money-moving calls (`create_order`, `pay_order`, `buy`/`buy_credits`,
  `send_usdc`, `send_zcash`, `load_core_credits`,
  `redeem_merchant_credits`) say that the call itself is not billed but
  moves real money — so a client model can tell the three cost tiers
  apart from the roster alone instead of guessing.

## 0.1.6

### Client-side tools (the norm agent work)

- **`/v1` client tool passthrough** — a chat request carrying its own
  `tools` runs in passthrough mode: the server roster is not advertised,
  the caller's definitions and `tool_choice` ride the listing verbatim,
  and the model's `tool_calls` return unexecuted with
  `finish_reason: "tool_calls"` (buffered and streaming). No `tools`
  field (or an empty array) keeps the original transparent server-side
  loop unchanged.
- **One-shot purchase tools on `/mcp`** — `tools/list` now also
  advertises `run_python` and every `provider_tool`-marked listing, and
  a call is one complete purchase (place, pay, poll, deliverable),
  mirroring `/v1`'s roster via the same helpers. Sanitized like every
  externally-reachable result.
- **Provider keys as `/mcp` bearers** — an `owk_` key authenticates
  `/mcp`, binding the session to its wallet and carrying `/v1`'s money
  rules: `spend` scope gates the explicit money tools, raw-address sends
  refuse for any provider key, and the key's daily budget covers `/mcp`
  purchases (up-front exhaustion gate, after-the-fact recording,
  settlement netting, atomic `buy` reservation). OAuth and local
  sessions are unchanged.
### Real usage and cost on `/v1` chat completions (fathom-x/norm#14)

- **`usage` on every chat completion**, buffered and streamed: OpenAI's
  `prompt_tokens` / `completion_tokens` / `total_tokens`, plus `cost`
  (USD, the convention OpenRouter set) and `charged_cents`, the
  authoritative integer — real money should not round-trip through a
  float. Streams emit it as a final choices-less chunk just before
  `[DONE]`, the same shape `stream_options.include_usage` produces, so
  an unmodified OpenAI-compatible client parses it without special
  casing. Previously the endpoint reported no usage at all, leaving a
  client to estimate cost from a token price list it has no way to know
  (norm's sidebar showed a flat `$0.00 spent`).
- **Cost covers the whole turn, not just the model calls.** Every order
  a completion places is charged against it — the OpenRouter turns *and*
  each tool call, which is a separately paid marketplace order. A tool
  call can cost far more than the inference around it (image
  generation), so a total that counted only model turns would understate
  real spend badly.
- The figure is the **settled** charge: a seller that meters and states
  `charged_cents` is billed at that, not at the gross deposit, mirroring
  the refund that already goes back to the key budget. The two read the
  same delivery, so the reported cost and the budget can never tell the
  user different stories about what a turn cost. A delivery that states
  no usage contributes zero rather than a guess.

## 0.1.5

No changes of its own. The `owallet-v0.1.5` tag was cut from 0.1.4's
tree, so its binaries report `0.1.4` — the version numbering resumes at
0.1.6 rather than reusing a published tag.

## 0.1.4

### Wallet status for norm's sidebar (fathom-x/overpay#415, fathom-x/norm#9)

- **`GET /v1/status`** — chain-free wallet status beside `/v1/models`:
  provider-key auth, balances and merchant credits projected through the
  same allowlist as the `get_balances` wallet tool, the calling key's
  daily-budget block, the wallet's configured `overpay_url`, and an
  `as_of` stamp in the wallet's timezone. The per-request spend
  allowance is deliberately absent — that ledger only exists inside a
  chat request. Powers norm's sidebar widget; poll on the order of a
  minute (the read hits the EVM RPC and Overpay live).

## 0.1.3

### Buyer-facing polish for metered orders (fathom-x/overpay#413)

- **`GET /health`** — unauthenticated liveness + version probe. norm's
  bootstrap compares it against the on-disk binary to restart a stale
  running serve (replacing the binary never touched the running
  process); serves predating the endpoint read as stale by the same
  logic.
- **`as_of` stamps on volatile `/v1` wallet-tool results**
  (`get_balances` / `list_orders` / `get_order_status`), in the
  wallet's timezone, plus point-in-time wording in the tool
  descriptions — so a model resending conversation history stops
  presenting an old balances snapshot as current.
- **Dashboard timestamps render in the wallet's timezone** with the
  zone abbreviation and a "3 minutes ago" hover title (purchases list,
  order detail, provider keys). The `timefmt` helpers live in
  owallet-mcp, shared by the dashboard and the `/v1` projections.
- **`settled_amount_cents` passes through the order projections** — a
  metered order's face value stays at the deposit, so wallets can now
  show what the buyer actually paid after settlement refunds.

## 0.1.2

### Key budgets net out metered settlement refunds

- **`/v1` place-and-pay now hands settlement refunds back to the
  provider key's daily budget.** The marketplace's OpenRouter inference
  listing meters each turn (OpenRouter cost + markup, settled against a
  prepaid deposit; fathom-x/overpay#412), and the delivered payload
  states the final `charged_cents`. Every pay site — buffered and
  streaming chat turns, `run_python` and listing-tool sub-orders, and
  the landing turn — releases the difference between the gross deposit
  recorded at pay time and that final charge, so daily budgets bound
  what a key actually cost instead of gross deposits. A failed turn
  (upstream error, `charged_cents: 0` plus a credit refund) returns its
  whole deposit; deliveries without `charged_cents` net nothing.
- **`install` resolves the per-env Overpay URL the way `serve` does**
  (synced from overpay), so `--staging`/`--dev` installs point the
  OpenCode provider entry at the right marketplace.

## 0.1.1

### Staging-capable release binaries (pre-public-release)

- **Release builds now compile with the `dev-envs` feature**
  (`owallet-release.yml`), so the binaries norm bundles accept
  `--dev`/`--staging`. Temporary for the pre-public-release phase —
  drop the feature flag from the workflow at the public release.
- **The staging environment has a built-in Overpay URL**
  (`defaults::OVERPAY_RAILS_URL_STAGING`,
  `https://overpay-eykm.onrender.com`), so `owallet --staging serve`
  works with no `OVERPAY_RAILS_URL_STAGING` in the environment — and no
  longer silently falls back to the **prod** Rails URL when that var is
  missing. Env vars still override the built-in.

### Listing-backed provider tools on `/v1` (generalizing `run_python`)

- **Any listing marked `metadata.provider_tool` is now a model-callable
  tool on `/v1`** — the generalization sketched in PR #383. The listing
  supplies everything: the marker's `name` becomes the function name, the
  listing description its description, and `buyer_note_schema` its
  parameters; executing a call places and pays a real order against the
  listing and feeds the delivered content back to the model (statuses,
  capped `delivered_content`, its type, and the download URL — an
  allowlist like every model-facing projection). First consumer: the
  weather reporter's `forecast` tool, closing the gap where a model
  narrates a purchase instead of performing one — one `forecast(input:
  "Galveston")` call is a real order end to end.
- Bare (non-object) `buyer_note_schema`s (`buyer_input :text`) are
  offered wrapped as `{input: <schema>}` and unwrapped at execution, and
  string buyer_notes now pass to the marketplace verbatim (matching the
  MCP `create_order` convention) instead of JSON-quoted.
- On the streaming path, a listing tool's in-flight partial output is
  forwarded into the chat stream as it arrives (unfenced — the preview
  is buyer-facing markdown — unlike `run_python`'s fenced stdout), so a
  streaming seller like the weather reporter pours its forecast into the
  client mid-call.
- The registry is cached per router (each serve env resolves its own
  marketplace's tools; a listing marked after startup needs a restart —
  same tradeoff as the listing-id caches). A failed catalog fetch
  degrades that request to `run_python` + wallet tools and retries on
  the next. Names are validated conservatively; the hardcoded
  `run_python` stays authoritative, and marked listings colliding with
  it or a wallet tool are skipped. Listing-tool orders count against the
  key's daily budget like every other order the endpoint pays.

### MCP responses sanitized of all on-chain data (fathom-x/overpay#391)

- **Every `/mcp` tool result is now projected through a chain-free
  allowlist** before it leaves the machine — both the `content` text the
  model reads and the `structuredContent` leg. Txids, tx hashes, wallet
  addresses, npubs, pubkeys, and account numbers no longer appear in any
  tool response; order/listing ids, amounts, statuses, and spending
  limits do. On-chain details stay on the surfaces the user reads
  directly: the CLI and the dashboard in their own browser.
- The `/v1` provider surface's allowlist projections moved to a shared
  `owallet-mcp/src/projection.rs`; `/v1` layers its spend-ledger context
  on top and the MCP transport applies `projection::sanitize` per tool
  via `tools::dispatch_sanitized`. Internal consumers that need raw
  shapes (`/v1`'s own projections, the dashboard's `sync_purchases`
  reuse) still call `dispatch` — sanitization is a property of the
  externally-reachable transport, not a mode flag. `OWALLET_MCP_UNSANITIZED=1`
  restores raw responses for local debugging.
- Envelope shapes are preserved (`{data: [...]}` etc.) so programmatic
  clients keep their contract; row keys converge on the `/v1` vocabulary
  (`order_id`, `listing_id`). `send_usdc`/`send_zcash` return
  `{status: "sent"}` instead of the tx id; `get_account_info` returns
  balances + credits + username with a pointer to the dashboard for
  addresses; `get_purchase` responses drop the raw `snapshot` (the local
  cache still stores it in full); free-text `balance_error` strings are
  scrubbed of hex runs (an allowlist can't reach inside a string).
- Leak tests cover every tool with payloads stuffed with all on-chain
  fields, at both the projection layer and the `/mcp` transport level.

### Model-callable wallet tools on `/v1`, gated by provider-key scopes

- **Wallet tools in the `/v1` tool loop.** Alongside `run_python`, the model
  can now call `get_balances`, `browse_marketplace`, `get_listing`,
  `list_orders` (find an order id when the user refers to "my pending
  order"), and `get_order_status` (any provider key) and — only with a
  spend-scoped key — the full purchase loop: `create_order` (unpaid),
  `buy_credits`, and `pay_order`. All eight
  are backed by the MCP tool handlers, but their results pass through
  **allowlist projections** before reaching the model: everything a tool
  returns is appended to `messages` and shipped to the OpenRouter seller on
  the next turn, so no on-chain data (txids, tx hashes, addresses) ever
  appears in a wallet tool result, in either direction — order ids,
  amounts, statuses, and spending limits only. Raw-address sends
  (`send_usdc` / `send_zcash`) are deliberately not offered on `/v1` and
  remain MCP/dashboard-only.
- **`pay_order` MCP tool** — settle a pending order with merchant credits,
  resolving the seller from the order's listing when `seller_slug` is
  omitted. (Credits are the API-side settlement path; the Rails API exposes
  no crypto payment address for pending orders.)
- **Provider-key scopes.** `provider_keys` gains a `scopes` column
  (`"chat"` / `"chat spend"`); every pre-existing key stays chat-only. The
  dashboard's create form has an "allow wallet spending" checkbox and the
  key list shows an Access badge. In the browser-login OAuth flow the
  consent page offers the same choice — and only that checkbox can grant
  `spend`: a client requesting `scope=provider spend` without the user's
  tick still gets a chat-only key.
- **Iteration cap raised to 10, with a graceful landing.** The old cap of 4
  predated the wallet tools (every iteration implied a paid `run_python`
  order) and broke the purchase loop mid-flight — browse → get_listing →
  create_order → pay_order is already four turns, and the request errored
  *after* creating and paying the order. Reaching the cap now runs one
  final `tool_choice: "none"` turn so the model reports what it actually
  did instead of the client getting a 502 with no record of the spend.
- **Per-request spend cap.** Wallet spending is additionally bounded per
  chat completion (default $20, `OWALLET_V1_SPEND_CAP_USD` to override):
  `buy_credits` reserves its amount up front, `pay_order` records what the
  redemption actually applied, and a request that hits the ceiling gets a
  tool-result error the model can relay rather than a failed request.
- **Per-key daily spending budgets.** A spend-scoped key can now carry a
  hard dollar budget per day instead of the all-or-nothing toggle:
  `provider_keys` gains `daily_budget_usd_cents` (NULL = no limit — every
  pre-existing key) plus `spent_usd_cents`/`spent_day` (the day's spend and
  its window; persists across requests). `buy_credits` reserves against
  today's budget with one atomic guarded UPDATE that also rolls a stale
  window over to the current day — parallel requests can't double-spend
  the last dollar, and midnight needs no sweeper job — and releases the
  reservation when the payment never moves; `pay_order` gates on remaining
  budget and records the redeemed amount after the fact. The budget is set
  in the dashboard's create form or the browser-login consent page (the
  field, like the `spend` scope itself, is the user's choice alone — a
  client can't request one), shown as "$X left today of $Y/day" in the key
  list, and editable in place with immediate effect; today's spend is
  never reset by an edit. `get_balances` reports the key's remaining daily
  budget alongside the per-request allowance so the model can plan; the
  wallet's own on-chain balance stays the outer bound shared by every key.
  (Per-task/per-session budget scopes are anticipated follow-ups — the
  window column is the piece that generalizes.)
- **The daily budget covers everything the key costs.** Field testing
  showed merchant credits draining while `spent_today` stayed $0.00:
  each `/v1` chat turn is itself a paid order, and the budget originally
  counted only the wallet spending tools. Every order the endpoint pays
  on a key's behalf (chat turns, `run_python`, the spending tools) now
  records against the key's daily budget; an exhausted key refuses new
  requests up front (HTTP 402, `insufficient_quota`), and exhaustion
  mid-request breaks to the `tool_choice: "none"` landing turn (one turn
  of accepted overshoot) so the model reports what it already did.
  Because operating spend applies to every key, budgets now attach to
  chat-only keys too — the dashboard create form, key list editor, and
  consent page offer the budget regardless of the spending checkbox.
  The per-request $20 cap is unchanged: it still bounds only what the
  spending tools move within one completion.
- **Dashboard-set per-request spending cap.** The `/v1` per-request
  ceiling on wallet-tool spending is now a wallet-level setting
  (`settings` key `v1_spend_cap_usd_cents`), edited from the OpenCode
  provider card and read per request — no server restart. Precedence:
  dashboard setting → `OWALLET_V1_SPEND_CAP_USD` env → the built-in $20
  default; blank clears the override. Deliberately wallet-level rather
  than per-key: keys are already individually bounded by their daily
  budgets, so the cap stays the shared blast-radius bound for any single
  chat completion.
- **Wallet timezone setting.** The budget day boundary follows a new
  wallet-wide IANA timezone preference (`settings` key `timezone`, default
  UTC; DST-correct via the bundled `time-tz` database) — set from the
  dashboard's "Time zone" card, validated server-side, blank resets to
  UTC. Rows are normalized at read time, so a stale window reads as a
  fresh budget the moment local midnight passes, and changing the
  timezone simply moves the boundary (a one-time early/late reset, never
  an accounting error). Timestamp *display* elsewhere stays UTC.

### OpenAI-compatible `/v1` endpoint (fathom-x/overpay#381)

- **`GET /v1/models` and `POST /v1/chat/completions`**, mounted alongside
  `/wallet`, `/mcp`, and `/oauth`. Lets a client that speaks the OpenAI Chat
  Completions API — most immediately, [OpenCode](https://opencode.ai) as a
  custom "provider" — use Overpay for inference with no Claude/OpenAI/etc
  API key of its own: the wallet's own stored Overpay auth pays for each
  request through the same merchant-credits flow as the `redeem_merchant_credits`
  MCP tool.
- **Hardcoded to the "OpenRouter Inference" listing**, per the issue. The
  model catalog is read live off that listing's own
  `buyer_note_schema.properties.model.enum` rather than duplicated in Rust,
  so it can't drift from the curated list on the Ruby side. The listing id
  itself can't be a literal constant (it's derived from the bot's private
  key and differs per environment), so it's resolved once by seller slug +
  title and cached for the process's lifetime.
- **Streaming reuses the `wait_for_order` machinery** — the same
  `partial_content` polling/diffing this release already added, reformatted
  as OpenAI `chat.completion.chunk` SSE frames. Falls back to the full
  delivered text as one catch-up chunk if an order finishes before the
  first poll ever observes a partial chunk, or if the streamed prefix fell
  short of what was ultimately delivered.
- **Agentic tool-calling, run entirely server-side.** Every turn advertises
  one tool — `run_python`, backed by the "Run Python Code" listing
  (`code_executor` bot, seller slug `exec`). When the model calls it, this
  endpoint executes it by placing and paying for a *second, real* Overpay
  order, feeds stdout/stderr back to the model, and loops until a turn
  produces no more tool calls — the HTTP caller never sees a `tool_call`,
  just a normal chat completion that happens to have run code along the
  way. The tool's JSON schema is the Python listing's own
  `buyer_note_schema`, read live for the same drift-avoidance reason as the
  model list. Caller-supplied `tools` are not accepted — this endpoint owns
  tool selection, since it's the one actually executing them. Each
  iteration that ends in a tool call is a real, separately-paid order on
  top of the OpenRouter order itself, so a hard cap
  (`MAX_TOOL_ITERATIONS = 4`) bounds a runaway conversation's real spend.
- **Requires a wallet-scoped provider API key** (`Authorization: Bearer
  owk_…`). Keys are minted from the dashboard's "OpenCode provider" card
  (create/list/revoke per wallet) and every request is pinned to the key's
  wallet; only a SHA-256 verifier is stored, so a copied database yields no
  spendable credential. Two ways to get one:
  - **Dashboard**: create the key on `/wallet` and paste it into the
    client (OpenCode keeps it in its own auth store).
  - **Browser login**: the local OAuth AS now honors a `provider` scope —
    the PKCE code exchange returns a freshly minted provider key instead of
    an `/mcp` access token. `owallet install --opencode-*` writes a
    generated auth plugin (`plugin/owallet.js` next to `opencode.json`)
    that drives this: `opencode auth login` gains a "Browser login" method
    that opens the consent page, catches the redirect on an ephemeral
    localhost listener, and stores the minted key. Keys from either path
    appear in the same dashboard list for revocation.
- **`owallet serve` prints the provider's own URL on startup**, alongside
  the existing dashboard/MCP/OAuth line, and **`owallet install
  --opencode-*` now also registers it as an OpenCode model provider** (not
  just the MCP tool source it already wired up) — fetching the model
  catalog live from the running server's `GET /v1/models` rather than
  hardcoding it a second time in the CLI. A server that isn't reachable at
  install time is a warning, not a hard failure: the MCP entry and any
  other install targets still get written, and the provider entry falls
  back to a single model, `"default"`, rather than being skipped.
- **`"default"` is always a valid model**, listed first by `GET /v1/models`
  and accepted by `/v1/chat/completions` without a live catalog check —
  exactly what lets `install` write a working provider entry even when it
  couldn't reach a server to fetch the real list. It's not a real
  OpenRouter model id (those are always `vendor/model-name`); the listing's
  own `coerce_model` resolves it to a concrete model the same way it
  already handles any unrecognized or stale id, before ever calling
  OpenRouter.

### Fixed: `install --opencode-global` wrote where OpenCode never looks (macOS)

- The global OpenCode target used `dirs::config_dir()`, which resolves to
  `~/Library/Application Support` on macOS — so `owallet install
  --opencode-global` silently wrote a config OpenCode never read, and left
  the real `~/.config/opencode/opencode.json` untouched. OpenCode follows
  the XDG layout on every platform, so this now resolves
  `$XDG_CONFIG_HOME/opencode` (when absolute) or `~/.config/opencode`
  directly. Linux was already correct — `dirs::config_dir()` agrees there;
  macOS is where the two diverge.

### `wait_for_order` streams the seller's output as it is generated

- When a seller publishes its work in progress (an LLM streaming tokens,
  say), each `notifications/progress` frame now carries the newly generated
  text in `data.delta` — only what is new since the previous frame — with
  the text itself as the human-readable message. A streamed
  `tools/call` therefore reads as the answer arriving rather than as a
  sequence of "still in flight" ticks.
- The buffer is read from `partial_content` on `GET /api/v1/orders/:id`.
  Polls that find nothing new fall back to the old status line, so an order
  from a seller that doesn't stream behaves exactly as before, and the final
  result is still the complete `delivered_content`.

### Fixed: CLI-authorized wallets got 401s from the MCP tools

- **Overpay bearers are keyed by the Overpay host, everywhere.**
  `owallet authorize` filed the bearer under the Overpay API URL, but
  `owallet serve` built its state with the local OAuth **issuer** URL as the
  token key — so a wallet linked from the CLI looked unlinked to the MCP
  tools, which fell back to NIP-98 and 401'd, until the user re-authorized
  through the dashboard. `AppState` and `McpState` now derive the key from
  their Overpay client (`OverpayClient::host_key()`), so the two can't drift.
- **`AppState::public_base_url`** carries the issuer URL that the dashboard's
  OAuth `redirect_uri` is built from — the role `host_key` was doing double
  duty for.
- **Existing tokens migrate on first read.** `Database::read_token_migrating`
  re-files a bearer stored under a legacy host key (the issuer URL, or a
  pre-normalization raw URL) onto the canonical key, so wallets linked
  through the dashboard before this fix keep working without re-authorizing.

### Zcash (Orchard-only) support

- **New `owallet-zcash` crate** built on librustzcash (the `zecrocks/zkv`
  wallet stack). A wallet can now receive, sync, show a balance of, and send
  Zcash using **Orchard-only Unified Addresses**, alongside the existing
  USDC/EVM rail. The same BIP-39 seed drives both chains.
- **Receive / sync / balance / send.** `generate` / `import` derive and cache
  the Orchard UA (offline); `import` gains `--zec-birthday <height>` to scan
  from an earlier height. `owallet send --asset zec --to u1… --amount N`
  broadcasts a shielded payment. New MCP tools `send_zcash` / `sync_zcash`.
- **Sync-on-read (like zkv) + manual sync.** The balance-display paths
  (`owallet account`, MCP `get_account_info`) and `send` auto-sync first,
  best-effort — the sync fast-path (compare chain tip, skip if unchanged)
  keeps repeat reads cheap, and an offline failure falls back to the
  last-known local balance. `owallet sync` and the `sync_zcash` MCP tool force
  a sync explicitly.
- **Overpay pay-with-Zcash.** The MCP `buy` flow routes to Zcash automatically
  when the server returns an Orchard UA + ZEC amount (`PurchaseCreditsResponse`
  gained `currency` / `crypto_address` / `payment_amount`). The dashboard
  `Send` form gained a USDC/ZEC asset selector.
- **Data files.** librustzcash's sqlite lives in the wallet's #310 per-`npub`
  state directory at `<data dir>/<npub>/zcash/` (e.g. `~/.owallet/<npub>/zcash/`,
  `0700`; `OWALLET_HOME` relocates the data dir, `ZEC_DATA_DIR` overrides just
  the Zcash base). So one backup of the owallet data dir captures the wallet DB,
  order cache, and Zcash sync state together. The wallet DB is stored unencrypted (matching the zkv
  reference; no SQLCipher/OpenSSL, so the static-musl release binary links
  cleanly): it holds the Unified *viewing* key + note metadata, which can't
  spend (the seed stays in owallet's encrypted DB). Proving parameters are
  **bundled into the binary** (no downloads), which increases binary size and
  build time noticeably.
- **Config.** `ZEC_NETWORK` (default `mainnet`), `ZEC_LIGHTWALLETD_URL`
  (default `zecrocks`), `ZEC_DATA_DIR`. Per-env suffixes supported under
  `serve`.
- **librustzcash kept current with the network.** The `zcash_*` crate set
  tracks the latest network-upgrade release train (e.g. `zcash_client_backend`
  0.23 / `zcash_client_sqlite` 0.21 / `zcash_keys` 0.14 / `zcash_primitives`
  0.28 / `zcash_protocol` 0.9 / `orchard` 0.14), so sync keeps working across
  consensus upgrades. Workspace `rusqlite` bumped to match the version
  `zcash_client_sqlite` resolves to (single `libsqlite3-sys`); MSRV 1.81.

### Per-wallet encrypted state directory (#310)

- **New per-`npub` state directory.** Groundwork for keeping all of a
  wallet's files together so "backing up a fully synced wallet" is just
  "copy the data directory". Layout: `<data dir>/<npub>/<artifact>`. The
  data dir co-locates with the DB file (the default `~/.owallet.db`
  yields `~/.owallet/`; a custom `OWALLET_DB_PATH` carries its state
  alongside it), and `OWALLET_HOME` overrides it outright.
- **Encrypted artifacts are bound to the wallet's own private key.** The
  general-purpose `owallet_db::WalletStateDir` (`write` / `read` /
  `exists` / `remove`, atomic writes, path-traversal guarded) AES-256-GCM
  encrypts each file under a key derived (HKDF-SHA256) from that wallet's
  secp256k1 private key — see `owallet_crypto::derive_state_key` and
  `Database::wallet_state(npub)` (derives the key from the unlocked seed).
  This is the storage substrate for upcoming chain sync-state (e.g.
  Zcash); no command wiring yet.
- **Order cache moved from SQLite into the state directory.** The
  per-wallet order/purchase cache now lives as plaintext JSON files at
  `<npub>/orders/<order_id>.json` instead of the `purchases` table. It is
  deliberately **not** encrypted (regenerable via `sync_purchases`, and
  kept readable without unlocking the DB). The `Database` purchase API
  (`upsert/list/read/delete/count_purchases`) is unchanged, so the MCP
  tools and `/wallet/purchases` dashboard are unaffected. Existing rows in
  the old `purchases` table are not migrated (the cache rebuilds on sync).

### Hide internal dev/staging environments from public builds (#312)

- **`--dev` / `--staging` are now gated behind the `dev-envs` Cargo
  feature** (off by default). Public release builds register only
  `--prod` / `--config PATH`; the `--dev` / `--staging` flags are absent
  from `--help` and rejected as unexpected arguments, and the non-public
  dev/staging URLs and ports are no longer compiled into the binary.
  Internal builds re-enable the full multi-environment behaviour with
  `cargo build --features dev-envs`.

### Final Python-parity port (last sync before Python sunset)

- **Per-environment URL env vars.** External URL overrides now require an
  env suffix matching the active config (`OVERPAY_RAILS_URL_PROD` /
  `_DEV` / `_STAGING`, same for `OVERPAY_PUBLIC_URL`); a bare unsuffixed
  `OVERPAY_RAILS_URL` is ignored when a config is active.
- **`OVERPAY_MCP_URL` removed.** owallet no longer calls the hosted
  Overpay MCP; `install` / `config --mcp` emit only the local `owallet`
  server entry.
- **24-word default + per-wallet password prompts.** `generate` mints 24
  words by default (CLI + web); `generate` / `import` (CLI + web) now set
  a per-wallet web-admin password when one isn't set. The DB-unlock
  prompt is relabelled "Database password". New `OWALLET_WALLET_PASSWORD`
  env var for non-interactive wallet-password supply.
- **Local purchase cache.** New `purchases` table mirrors delivered /
  terminal orders per wallet. `get_order_status` / `wait_for_order` cache
  terminal orders and strip `delivered_content` over 2 KB to a pointer
  (new `include_delivered_content` arg keeps it inline). New MCP tools
  `list_purchases`, `get_purchase`, `sync_purchases`, and a
  `/wallet/purchases` dashboard (list + detail + sync).
- **`list_marketplace`** now flattens each listing's `delivery_eta`
  object to a scalar `delivery_eta_seconds` (p50), matching Python.

### Parity work — closes the last gaps with the Python implementation

- **Three MCP tools are no longer stubbed.**
  - `get_merchant_credits` — lists merchant credit balances; with a
    `seller_slug` returns just that seller's row.
  - `redeem_merchant_credits` — applies stored credits to an order
    (takes `seller_slug` + `order_id`).
  - `buy` — two-step compose: opens a credit-purchase order with
    Overpay, then signs + broadcasts a USDC transfer to the returned
    payment address. Returns `order_id`, `tx_hash`, and the order URL.
  All three honour the same Bearer-or-NIP-98 fallback the rest of the
  MCP surface uses.
- **Dashboard `POST /wallet/send` is real.** The form button no longer
  reads "alloy integration ships in a later phase"; submitting renders
  a result page with the tx hash + explorer URL on success or an
  error notice on failure. Includes a `confirm()` warning that the
  send is irreversible.
- **Browser-initiated Overpay OAuth from the dashboard.** Three new
  routes mirroring the Python flow:
  - `GET /wallet/overpay-login` — opens an Overpay-hosted web session
    using the stored bearer.
  - `GET /wallet/authorize` — kicks off PKCE against Overpay; stores
    pending state in a short-lived `DashMap` keyed by an HttpOnly
    cookie.
  - `GET /wallet/authorize/callback` — exchanges the code, stores the
    bearer under `(npub, host_key)`, refreshes the cached username.
- **On-chain balances** in three places: the `get_account_info` MCP
  tool returns `"balances": {chain, eth, usdc}`; `owallet account`
  prints `eth: N ETH` / `usdc: N USDC`; the dashboard adds two rows
  to the account table. Each lookup is best-effort — RPC failures
  surface in the rendered output but never block the rest of the
  response.
- New `owallet_evm::eth_balance(rpc_url, account)` helper.
  `owallet_evm::format_amount` is now `pub`.

## 0.0.1 — initial Rust release

Drop-in replacement for the Python `owallet/` implementation, with the
same `~/.owallet.db` encrypted-DB format and the same CLI / dashboard /
MCP surface.

### CLI

- `init`, `generate`, `import`, `select`, `export key`, `account` —
  wallet bookkeeping commands. `import` accepts both BIP-39 mnemonics
  and hex private keys; `export key` re-derives from the stored seed
  and offers `hex`, `hex0x`, and `mnemonic` formats.
- `authorize`, `login` — Overpay OAuth 2.0 PKCE link + one-time
  Overpay web-session URL. `authorize` binds an ephemeral axum
  callback server on a free port, opens the browser, exchanges the
  code, persists the bearer token.
- `account` — wallet metadata plus a live Overpay fetch. Uses a stored
  bearer token if one exists; otherwise falls back to a NIP-98
  envelope signed by the wallet key.
- `list marketplace` — paginated Overpay listings.
- `send --to ADDR --amount USDC` — ERC-20 USDC transfer on the
  configured EVM chain (default Base mainnet).
- `serve` — runs one HTTP server per active `.owallet` config in a
  single tokio runtime. Each server exposes the dashboard at `/wallet`,
  the OAuth Authorization Server under `/oauth/*` +
  `/.well-known/oauth-authorization-server`, and the MCP transport at
  `/mcp`. Shared encrypted DB, per-server `SessionStore`. Combine
  `--prod/--dev/--staging` + comma-separated `--port` overrides for
  positional binds.
- `install --{claude,opencode,codex}-{local,global}` — writes MCP
  client config entries. Codex TOML goes through `toml_edit` so
  user-authored comments and the relative ordering of other sections
  survive the edit.
- `config [--mcp]` — show resolved URL config or print the install JSON.

### MCP tools

10 tools mirroring the Python `wallet_mcp.server` surface:
`get_account_info`, `list_marketplace`, `get_wallet_orders`,
`create_order`, `get_order_status`, `wait_for_order`,
`get_merchant_credits`, `redeem_merchant_credits`, `buy`, `send_usdc`.
NIP-98 fallback applies to every tool that talks to Overpay.

Note: in the original 0.0.1 release `buy`, `redeem_merchant_credits`,
and `get_merchant_credits` shipped as honest stubs; they were wired up
in the Unreleased parity work above.

### Crypto / wire format

- Byte-compatible with the Python `wallet_mcp.db` on-disk format:
  PBKDF2-HMAC-SHA256 (600k iterations) → AES-256-GCM with 16-byte
  nonces, tag appended to ciphertext, identical schema + migrations.
- BIP-39 mnemonics (12 or 24 word) + BIP-32/44 derivation at
  `m/44'/60'/0'/0/0`.
- Nostr `npub` via bech32 of the BIP-340 x-only public key.
- NIP-98 HTTP auth as `Authorization: Nostr <b64>`, with Schnorr
  signatures verified by the matching test.
- USDC transfers via `alloy` 0.7 with auto-filled nonce + gas +
  EIP-1559 fees.

### Local OAuth 2.0 Authorization Server

Backed by the encrypted DB. RFC 7591 dynamic client registration, PKCE
S256, `/consent` page password-gated per wallet. The MCP transport
validates bearer tokens against the AS's `access_tokens` table and
resolves the token's `npub` to scope the call.

### Not in 0.0.1

- `migrate` from the legacy macOS keychain + `~/.wallet_tokens.json`
  layout. Re-import seeds manually via `owallet import`.
- Cashu (ecash) wallet — staged for a future `--features cashu` build.
- x402 listing-buy through a public facilitator — the MCP tool is a stub.

### Packaging

- Single static binary per target. Static musl Linux, dynamic Apple
  arm64 + x86_64, Windows MSVC. Built by
  `.github/workflows/owallet-rs-release.yml` on every `owallet-rs-v*`
  tag.
- Distroless Dockerfile (replaces the conda + constructor pipeline).
