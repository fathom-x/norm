# CLAUDE.md — owallet

Operational notes for working in this workspace — the `owallet/`
directory of norm (the opencode fork; see the repo-root CLAUDE.md).
This workspace is the standalone fork of the `owallet-rs/` workspace
from `fathom-x/overpay` (the marketplace it talks to — that repo's
top-level CLAUDE.md covers the Rails app + bot_manager).

## What this is

Rust port of the Python `owallet/` package from the `fathom-x/overpay`
repository. The on-disk encrypted DB (`~/.owallet.db`) is intentionally
**byte-compatible** with the Python implementation — same schema, same
PBKDF2/AES-GCM parameters, same migration list. Don't change the wire
format without also updating the Python compat fixture + test.

## Workspace layout

```
crates/
  owallet-crypto/   AES-256-GCM, PBKDF2-SHA256, BIP-39/32, Nostr, NIP-98
  owallet-db/       rusqlite + Database; reads existing Python DBs
  owallet-config/   .owallet dotenv resolution, --prod/--dev/--staging
  owallet-overpay/  reqwest client for the Rails API + PKCE helper
  owallet-evm/      alloy 0.7 wrapper (ERC-20 USDC + chain table)
  owallet-zcash/    librustzcash wrapper (Orchard-only: receive/sync/balance/send)
  owallet-mcp/      JSON-RPC 2.0 MCP transport + tool registry
  owallet-http/     axum router: dashboard + OAuth AS + /mcp mount
  owallet/          binary crate (clap CLI)
```

Original Python source lives in the `fathom-x/overpay` repository at
`owallet/wallet_mcp/`. Cross-references in code comments use
`wallet_mcp/server.py:NNNN` style.

## Syncing from overpay

While `owallet-rs/` still lives in overpay, pull its newer commits into
this repo with `scripts/sync-from-overpay.sh` (fetch → deterministic
`git subtree split` → merge; see the script header for why the histories
stay compatible). Conflicts only arise in files norm has diverged on
(README.md, CLAUDE.md, the vendored
`crates/owallet-crypto/tests/fixtures/nip98_vectors.json`) — resolve
keeping norm's standalone wording, and re-copy the fixture if overpay's
`test/fixtures/nip98_vectors.json` changed. Run the test suite after
every sync. This is deliberately **not** a submodule: a submodule can
only pin the whole overpay repo, and wouldn't merge upstream changes
into these crates.

## Run commands

```bash
# MUST cd into this workspace; cargo can't find Cargo.toml from the
# repo root (that's the opencode fork).
cd owallet

cargo build --workspace          # ~30s clean, instant incremental
cargo test --workspace           # ~6 min cold (alloy build dominates)
cargo clippy --workspace --all-targets -- -D warnings   # CI runs this
cargo fmt --all --check          # also CI; `cargo fmt --all` to apply
cargo build --profile dist -p owallet  # release-grade binary; ~3 min
```

Single-binary smoke test:

```bash
TMP=$(mktemp -d) OWALLET_PASSWORD=pw OWALLET_DB_PATH=$TMP/test.db \
  ./target/debug/owallet init \
  && ./target/debug/owallet import --mnemonic "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about" \
  && ./target/debug/owallet account
```

## Conventions that aren't obvious from the code

- **Database byte format.** Don't touch `owallet-crypto::kdf` or
  `owallet-crypto::aesgcm` without re-running the Python compat test
  (`cargo test -p owallet-db --test python_compat`). PBKDF2 iters
  = 600,000 for the AES key, count=1 for the verify hash. AES-GCM uses
  **16-byte nonces** (PyCryptodome default), not 12 — see
  `Aes256Gcm<U16>` in `aesgcm.rs`.

- **NIP-98 fallback.** The `Auth` enum on `OverpayClient` has three
  variants (`None`, `Bearer`, `Nip98(&PrivateKey)`). Any tool /
  command that takes an Overpay action should go through
  `McpState::resolve_owned_auth()` (for tools) or the
  bearer-or-derive-key branch in `commands/account.rs` (for the CLI),
  so users without a stored token still authenticate via wallet key.

- **`/v1` OpenAI-compatible endpoint** (`owallet-mcp/src/openai_compat.rs`,
  mounted by `owallet-http`). Hardcoded to exactly two *listings*
  — "OpenRouter Inference" (seller `openrouter-bot`, chat) and "Run Python
  Code" (seller `exec`, the one listing-backed tool) — resolved via
  `resolve_listing_id_cached(state, seller_slug, title, cache)`.
  **The listing-tool side is generalized** (`ListingTool` /
  `listing_tools` / `run_listing_tool`): any listing whose
  `metadata.provider_tool.name` is set (the bot DSL's
  `provider_tool name: "..."` — `Listing#metadata` as the behavior gate,
  cf. `metadata["service"]`; Rails exposes it as the curated top-level
  `provider_tool` field on the public listing JSON, like
  `delivery_eta` — the raw metadata hash never rides the API) is
  advertised as a tool — name from the marker, description from the
  listing, `buyer_note_schema` as `function.parameters` (fetched via a
  per-listing `show`, since the index deliberately omits the heavy
  schemas) (bare non-object schemas are wrapped as
  `{input: …}` and unwrapped at execution; string buyer_notes pass to
  Rails verbatim, matching MCP `create_order`). Execution is
  place-and-pay + poll-to-terminal, result projected through an
  allowlist (`extract_listing_delivered`, content capped at
  `DELIVERED_CONTENT_MODEL_CAP`). The registry cache lives **on `Ctx`**,
  not a process global — each serve env resolves its own marketplace and
  tests stay isolated; a marked-after-startup listing needs a restart. A
  failed catalog fetch is not cached (that request degrades to
  `run_python` + wallet tools). The hardcoded `run_python` path is still
  authoritative for its name — marked listings colliding with it or a
  wallet tool are skipped — and its partial-output streaming remains
  special-cased in the streaming generator (fenced, code-shaped); generic
  listing tools forward their in-flight partial output too, unfenced —
  the preview is buyer-facing markdown — set off by blank lines, with
  keep-alive comments between deltas. First consumer: the weather reporter's
  `forecast`.
  **Wallet tools** (`WALLET_TOOLS` in `openai_compat.rs`) sit alongside
  the listing tool: `get_balances` / `browse_marketplace` / `get_listing`
  / `list_orders` / `get_order_status` for any provider key;
  `create_order` / `buy_credits` / `pay_order` only when the key's scopes
  include `spend` — together the spend set is the full purchase loop
  (find → order → settle with credits), all order-id-scoped. They're backed by `crate::tools::dispatch` but carry their own
  model-facing definitions and **allowlist projections**
  (`project_balances` & co.): every tool result lands in `messages` and
  ships to the OpenRouter seller inside the next turn's `buyer_note`, so
  no on-chain data (txid, tx hash, address) may ever appear in a wallet
  tool result — order ids, amounts, statuses, and spending limits only.
  When touching these, extend the projections field-by-field (never copy
  whole payload objects) and keep raw-address sends (`send_usdc` /
  `send_zcash`) off this surface entirely — they're MCP/dashboard-only by
  design.
  **The same rule now covers the MCP transport** (fathom-x/overpay#391):
  the row-level allowlists live in `owallet-mcp/src/projection.rs`
  (shared with `/v1`, which layers its spend-ledger fields on top), and
  the `/mcp` transport calls `tools::dispatch_sanitized` — dispatch, then
  `projection::sanitize(tool, data)`, then re-render `text` from the
  *projected* data, so neither `content` nor `structuredContent` can
  carry chain data. Internal consumers that need raw shapes (`/v1`'s own
  projections, the dashboard's `sync_purchases` reuse) call plain
  `dispatch`; sanitization is a property of the externally-reachable
  transport, not a flag on the shared `McpState`
  (`OWALLET_MCP_UNSANITIZED=1` exists for local debugging only).
  Sanitized envelopes keep their shapes (`{data: [...]}`), row keys use
  the `/v1` vocabulary (`order_id`/`listing_id`), and free-text
  `balance_error` strings get hex runs scrubbed (`scrub_hex`) since an
  allowlist can't reach inside a string. Renderers in `render.rs` accept
  both raw and sanitized shapes — internal callers still render raw.
  When adding a tool, add its projection arm to `sanitize` and a stuffed
  payload to the leak test in `projection.rs`.
  Spending is bounded per request by `SpendLedger`. The cap
  resolves per request via `effective_spend_cap`: the wallet-level
  dashboard setting (`Database::read_spend_cap_usd_cents`, settings key
  `v1_spend_cap_usd_cents`, edited from the OpenCode provider card —
  wallet-level on purpose; keys are already bounded by their daily
  budgets) → env `OWALLET_V1_SPEND_CAP_USD` → `DEFAULT_SPEND_CAP_USD`
  ($20). For the tools it bounds:
  `buy_credits` reserves up front, `pay_order` records the redeemed
  amount after the fact. On top of that sits the **per-key daily
  budget** (`provider_keys.daily_budget_usd_cents`, NULL = no limit —
  every pre-existing key; `spent_usd_cents` is scoped by `spent_day`, the
  julian day of the current date **in the wallet's timezone**). The daily
  budget bounds **everything the key costs**, not just the spending
  tools: `place_and_pay_order` records each chat turn's and `run_python`
  execution's redemption against it, so it applies to (and is offered
  for) chat-only keys too. An exhausted key 402s new requests up front
  (`exhausted_key_budget`); exhaustion mid-request breaks the loop to the
  landing turn — one accepted turn of overshoot. For the spending tools,
  `buy_credits` reserves against today's window via
  `Database::try_reserve_provider_key_spend` — one atomic guarded UPDATE
  that also rolls a stale window over to the current day, so concurrent
  requests on one key can't double-spend the last dollar and local
  midnight needs no sweeper job — and `pay_order` gates on remaining
  budget, then records the redemption after the fact (so it can overshoot
  by one final redemption; same soft semantics as the ledger). The day
  boundary follows the wallet-wide **timezone setting** (`settings` key
  `timezone`, IANA name, default UTC; `time-tz` bundles the tz database
  and is compatible with the workspace's `time =0.3.37` pin) — set from
  the dashboard's "Time zone" card (`POST /wallet/settings/timezone`),
  validated by `owallet_db::timezone_is_valid`; it governs the budget
  window and dashboard timestamp display (`owallet-http/src/timefmt.rs`
  renders wallet-local times with the zone abbreviation and a relative-age
  hover title). Rows are
  normalized at read time (the SELECT zeroes spend from a past window),
  so `spent_usd_cents` on a `ProviderKeyRow` is always *today's* spend —
  but keep call sites on `spent_today_usd_cents()` /
  `remaining_today_usd_cents()` anyway; the raw column (readable via SQL)
  still holds the last-spend-day value. Budget refusals are tool-result
  errors the model relays, not failed requests. The budget is set at mint
  time (dashboard create form / consent page — the user's choice alone,
  like the `spend` scope; `parse_budget_usd` in `dashboard/provider.rs`
  is the one parser all three surfaces use) and edited in place via
  `POST /wallet/provider-keys/budget` with immediate effect; today's
  spend is deliberately never reset by an edit. The wallet's on-chain
  balance stays the outer bound shared by every key. Per-task/session
  budget windows are an anticipated generalization (pending design input)
  — extend the window keying rather than adding a parallel accounting
  path.
  The MCP `pay_order` tool (tools.rs) is the
  underlying primitive — credits are the *only* API-side way to settle an
  order (the Rails API exposes no crypto payment address for pending
  orders), which is what makes the model-facing vocabulary naturally
  chain-free. Both the model list (`GET /v1/models`) and
  the `run_python` tool's JSON schema (`run_python_tool_def`) are read
  live off their listing's own schema via `get_listing_value` rather than
  duplicated in Rust — changing either listing's Ruby side
  (`MODEL_OPTIONS`, `buyer_note_schema`) needs no Rust change to match,
  and any general router should preserve that property.
  `partial_output`/`new_output_since`/`WAIT_TERMINAL_STATUSES` in
  `tools.rs` are `pub(crate)` specifically so this module can reuse
  `wait_for_order`'s streaming-diff logic rather than reimplementing it.
  **The tool loop is server-side and transparent**: a tool call never
  reaches the HTTP caller — `run_agentic_loop` (buffered) / the streaming
  generator's own copy of the same loop shape executes `run_python` as a
  *second, real, separately-paid* order and feeds the result back for
  another OpenRouter turn, capped at `MAX_TOOL_ITERATIONS` iterations (10 —
  the full purchase loop needs five-plus, so don't tighten it back to the
  old 4 without re-checking that flow) and, for the wallet spending tools,
  by the per-request `SpendLedger` dollar ceiling (the two are
  complementary: iterations bound the endpoint's own chat/run_python
  costs, dollars bound what the model can move). Reaching the cap does
  **not** fail the request: real orders may already be created and paid by
  then, so both loops land on one final `tool_choice: "none"` turn that
  forces the model to report what it did; only a landing turn with no text
  at all yields the safety-cap error. A failed tool
  execution becomes an `{"error": ...}` tool-result message fed back to
  the model rather than aborting the request — see `execute_tool_call`'s
  doc comment for why. The request timeout (120s) and poll cadence (1s)
  are construction-time values, not request parameters — real OpenAI
  clients have no way to ask a server for a different timeout, so this
  doesn't either — env-overridable for ops (`OWALLET_V1_TIMEOUT_S`,
  `OWALLET_V1_POLL_MS`, `OWALLET_V1_FALLBACK_POLL_MS`); tests use the
  private `router_with_timing` / `router_with_config_full` knobs and the
  `Ctx` wrapper struct (kept separate from `McpState` on purpose — these
  fields are HTTP-specific and don't belong on the struct MCP tool calls
  share). **Streaming is push-first**: `OrderFollower` (the one shared
  turn-follow loop — the passthrough/agentic/landing copies were
  unified) subscribes to the order's `payment_status` ActionCable topic
  (`owallet_overpay::cable`, anonymous — the order UUID is the
  credential, same as the order page) and applies delta frames as they
  arrive; the poll drops to the fallback cadence as a safety net,
  `since_seq` makes every poll conditional (the marketplace omits an
  unchanged partial buffer), a seq gap or refresh frame triggers an
  immediate conditional GET, and any socket failure downgrades to the
  plain polling loop. `OWALLET_V1_WS=0` disables the socket outright.
  `place_and_pay_order` settles in the create call when the marketplace
  supports `pay: "merchant_credits"` (response carries a `payment` key),
  falling back to the separate redeem round trip when it doesn't.
  The two hardcoded listing-id caches live on `McpState::listing_ids`
  (per serve env, like the `Ctx` listing-tool registry) — they were
  process globals once, which cross-contaminated multi-env serves. `owallet install --opencode-*`
  (`crates/owallet/src/commands/install.rs`) writes an OpenCode `provider`
  entry pointed at this endpoint, fetching the model list the same
  drift-avoidance way — `fetch_models` reads the OpenRouter listing's own
  `buyer_note_schema` straight off Overpay, *not* a second hardcoded copy
  and *not* the running server's `/v1/models`, so `owallet serve` does not
  have to be up for `install` to work. An unreachable Overpay is a warning,
  not a hard failure: `build_provider_entries` still writes a provider
  entry, with just `DEFAULT_MODEL` in it. It deliberately writes **no**
  `options.apiKey` — OpenCode prompts for the key and keeps it in its own
  auth store (`~/.local/share/opencode/auth.json`), so a placeholder there
  is at best inert and at worst overrides the real key; a key the user set
  by hand is preserved. `/v1` requires a wallet-scoped `owk_` provider key
  (hashed in the `provider_keys` table; dashboard card on `/wallet` creates
  and revokes them, and requests are pinned to the key's wallet). Keys
  carry `scopes` ("chat" / "chat spend"; NULL rows from before the column
  are chat-only) — `spend` unlocks the wallet spending tools and is
  granted only by an explicit user choice: the dashboard create form's
  checkbox, the consent page's checkbox in the browser-login flow, or the
  CLI's `--spend` flag (`owallet provider-key create`, whose `--json`
  output exists for norm's non-interactive bootstrap)
  (`consent_post` strips any client-*requested* `spend` scope, so an
  OAuth client can't pre-grant itself spending power).
  `install --opencode-*` also drops a **generated auth plugin**
  (`plugin/owallet.js` beside the target `opencode.json`, template
  `OPENCODE_AUTH_PLUGIN_RUNTIME` in `install.rs`; OpenCode auto-discovers
  `{plugin,plugins}/*.{ts,js}`, every export must be a plugin function) so
  `opencode auth login` offers "Browser login": PKCE against the local
  OAuth AS with `scope=provider`, whose token exchange mints a provider key
  instead of an `/mcp` access token (`oauth_as.rs`). The plugin's callback
  must clear its race timer and close its localhost listener — either one
  left live pins the one-shot `opencode auth login` process (found the
  hard way; there's a node-driven E2E recipe in the session notes).
  `--opencode-global` resolves
  `$XDG_CONFIG_HOME/opencode` or `~/.config/opencode` — **not**
  `dirs::config_dir()`, which on macOS is `~/Library/Application Support`
  where OpenCode never looks. The file is edited through jsonc-parser's
  CST (the JSON analogue of the Codex writer's `toml_edit`) because
  OpenCode's config is JSONC and users comment blocks out by hand; a
  strict `serde_json` read rejects a file OpenCode itself accepts, and a
  `to_string_pretty` rewrite would silently delete every comment.
  **`DEFAULT_MODEL` (`"default"`)** is a sentinel model id, always listed
  first by `GET /v1/models` and always accepted by `validate_request`
  without a live catalog fetch — no real OpenRouter id can collide with it
  (those are always `vendor/model-name`). It's forwarded to the listing's
  `buyer_note.model` unchanged; `OpenrouterInferenceListing#coerce_model`
  (Ruby) is what actually resolves it to a concrete model, the same way it
  already treats any string outside its own `MODEL_OPTIONS`. `install`'s
  copy of this constant (`crates/owallet/src/commands/install.rs`) is a
  plain literal, not a cross-crate import — keep the two in sync by hand
  if this ever changes.

- **host_key vs public_base_url.** Overpay bearers live in `tokens`
  under `(npub, host_key)`, where `host_key` is always the **Overpay
  API** URL — `owallet_overpay::host_key(rails_url)`, or equivalently
  `OverpayClient::host_key()`. `AppState`/`McpState` derive it from
  their Overpay client rather than accepting it as an argument, so a
  wallet linked with `owallet authorize` and one linked through the
  dashboard resolve to the same row. `AppState::public_base_url` is a
  separate field holding this dashboard's **issuer URL**, used only to
  build the OAuth `redirect_uri`. Serve used to conflate the two and key
  bearers by the issuer, so CLI-linked wallets 401'd from MCP tools;
  `Database::read_token_migrating` reads through
  `AppState::legacy_host_keys` and re-files those rows. When wiring
  tests, `write_token` under the *Overpay mock's* URI, not the issuer.

- **clap subcommand for serve / install.** Both consume the global
  `--prod/--dev/--staging` flags directly (multi-config support).
  `commands::mod::dispatch` short-circuits them before
  `args.command` is moved by the main match.

- **AppState::fork_with_fresh_sessions().** Multi-env serve uses one
  `Arc<Mutex<Database>>` shared across servers but a separate
  `SessionStore` per server — so a dashboard login on dev doesn't grant
  admin on prod. Use this any time you want to spawn another server
  sharing the same DB.

- **Per-env URL vars.** `OVERPAY_RAILS_URL` / `OVERPAY_PUBLIC_URL` are
  read unsuffixed by the code, but env *inputs* must be suffixed
  (`_PROD`/`_DEV`/`_STAGING`). `owallet_config::apply_env_overrides`
  (single-config, called in `cli::load_env_from_flags`) and
  `serve::server_from_dotenv` (per-config) resolve the suffixed form.
  `OVERPAY_MCP_URL` is gone — don't reintroduce it.

- **Per-wallet password (`generate`/`import`).** Prompts via
  `password::read_new_wallet_password`, which honors
  `OWALLET_WALLET_PASSWORD` (distinct from the DB `OWALLET_PASSWORD`).
  The env fallback exists because `rpassword` reads `/dev/tty`, not
  piped stdin — so `assert_cmd` tests can't drive the prompt; set the
  env var instead (the CLI test helper does).

- **Purchase cache.** `owallet-db`'s `purchases` table (plaintext, no
  unlock needed) + `upsert/list/read/delete/count_purchases`. The MCP
  `get_order_status`/`wait_for_order` cache terminal orders and strip
  `delivered_content` >2 KB; `list/get/sync_purchases` tools + the
  `/wallet/purchases` dashboard read it. Timestamp coercion (int or
  ISO-8601 → unix) uses the `time` crate (parsing feature) in
  owallet-db; owallet-http formats dashboard
  timestamps in the wallet's timezone via `timefmt.rs` (`time` +
  `time-tz`).

## Zcash (owallet-zcash)

- **Orchard-only, librustzcash.** Adapted from `zecrocks/zkv`
  (`/home/user/zkv`). Receive = an Orchard-only Unified Address derived
  from the same BIP-39 seed (`owallet_crypto::bip39_seed_from_stored` →
  `UnifiedSpendingKey::from_seed` → `default_address(UnifiedAddressRequest::ORCHARD)`).
  Send = ZIP-321 propose/create with `StandardFeeRule::Zip317`,
  `ShieldedProtocol::Orchard`, `LocalTxProver::bundled()`.

- **Data files live in the wallet's #310 per-`npub` state dir.**
  `owallet_zcash::data_dir_for(npub)` → `<data dir>/<npub>/zcash/` (`0700`),
  rooted at `owallet_db::wallet_state_dir(npub)` (owallet-zcash depends on
  owallet-db for this). `OWALLET_HOME` relocates the whole data dir;
  `ZEC_DATA_DIR` overrides just the Zcash base (`<ZEC_DATA_DIR>/<npub>/`). So a
  backup of the owallet data dir captures the wallet DB + order cache + Zcash
  state together. The wallet
  DB (`data.sqlite`) is stored **unencrypted** (rusqlite `bundled`, no
  SQLCipher/OpenSSL — keeps the static-musl release binary clean; matches
  zkv's posture). It holds the account's Unified *Full Viewing* Key + note
  metadata — privacy-sensitive but not spend-capable (the spending key is
  derived on demand from the seed in owallet's encrypted DB), and it sits in
  a `0700` dir. So `sync`/`zec_balance`/`open_wallet_db` take no seed — only
  `send_zcash` and account init need it (spending key / `create_account`).
  Proving params are bundled (`zcash_proofs/bundled-prover`) — nothing
  downloaded, but the binary + build time grow noticeably.

- **Non-Send futures behind axum.** librustzcash futures hold non-`Send`
  state (the rusqlite `WalletDb`, gRPC client, prover) across awaits, so
  they can't be awaited directly inside an axum (`Send`-future) handler.
  The MCP tools and the HTTP dashboard run them via `spawn_blocking` + a
  per-call current-thread runtime (`tools::blocking_zcash`,
  `dashboard/send::send_zec`). The CLI uses `block_on` (current-thread,
  `enable_all`) so it's fine directly.

- **Version pins.** The `zcash_*` set is a lockstep release train pinned
  to zkv's versions (do not mix minors). `zcash_client_backend 0.22` pins
  `time-core =0.1.2`, so the whole workspace pins `time =0.3.37`. And
  `zcash_client_sqlite` needs `rusqlite 0.37`, so the workspace bumped off
  0.31 — there can be only one `libsqlite3-sys` (`links = "sqlite3"`).
  owallet-zcash's rusqlite enables `bundled-sqlcipher` (links system
  OpenSSL); features unify across the shared `libsqlite3-sys`.

- **Sandbox can't reach lightwalletd.** gRPC-over-TLS to `zec.rocks:443`
  fails under the egress-inspection CA exactly like reqwest (see below), so
  live `sync`/`send` only work outside the sandbox or against a local
  plaintext lightwalletd. Offline paths (UA derivation, balance read,
  data-dir layout, amount formatting) are unit-tested and do work in-sandbox.

## API quirks that cost me time

- **secp256k1 0.29.** Uses `SecretKey::from_slice(&[u8])`,
  `XOnlyPublicKey::from_slice(...)`, `schnorr::Signature::from_slice`.
  The `from_byte_array` API exists in later versions but not 0.29 —
  pin matters.

- **alloy 0.7 ProviderBuilder.** Must call
  `.with_recommended_fillers()` before `.wallet(...)` for
  `send_transaction` to auto-populate nonce / gas / EIP-1559 fees.
  Without it you get a "missing properties" `local usage error`. See
  `crates/owallet-evm/src/usdc.rs`.

- **askama 0.12.** Templates live in `crates/owallet-http/templates/`
  and are picked up via `#[derive(Template)]` `#[template(path = ...)]`.
  Adding a new template file means re-running `cargo build` to pick it
  up via the proc-macro. Variables must implement `Display` (escape via
  `{{ var|safe }}` for raw HTML).

- **rusqlite migration errors.** The migration list in
  `owallet-db/src/schema.rs` swallows "duplicate column name" errors so
  applying twice is idempotent. Don't add a non-additive migration
  without thinking through the existing fixture DB.

## Test gotchas

- **Cwd-mutating tests.** `commands::serve::tests` use a `ChdirGuard`
  to point `resolve()` at a tempdir; this is process-wide so the tests
  are annotated `#[serial_test::serial]`. Don't remove the annotation
  without restructuring — the failure shows up as flaky test runs only
  under parallelism. Same applies if you add similar tests in
  `commands::install::tests`.

- **Python compat fixture.** `crates/owallet-db/tests/fixtures/python_v0_1_0.db`
  is generated by the sibling `generate_python_v0_1_0.py` script. The
  `python_v0_1_0` name refers to the Python `wallet_mcp` package
  version (still 0.1.0 upstream), NOT this Rust port's own version.
  Don't rename when you bump the workspace version. To regenerate:
  `pip install cryptography && cd crates/owallet-db/tests/fixtures &&
  python3 generate_python_v0_1_0.py`.

- **wiremock + alloy JSON-RPC.** `crates/owallet-evm/tests/send_test.rs`
  has a hand-rolled `JsonRpcMock` that returns canned responses for the
  ~10 RPC methods alloy fires during a sendTransaction → receipt cycle.
  If alloy adds a new auto-filler that calls another method, the mock
  needs the new branch or the test hangs. The same mock is
  **deliberately duplicated** into `owallet-http/tests/mcp_test.rs`
  and `owallet-http/tests/http_test.rs` (each prefaced with a
  cross-reference comment). Extracting into a shared
  `pub mod test_support` behind a feature gate was tried but broke
  `cargo test --workspace` (the gated integration test silently
  skipped); three 50-line copies in lockstep is the lesser evil.

- **axum-test 17.** Use `.form(&json!({...}))` to send form-encoded
  bodies; `.text(...)` with manual `Content-Type` does NOT work
  (returns 415). Cookie persistence requires `server.save_cookies()`
  (not `do_save_cookies` — that's a newer version).

## Clippy nits to expect

These are all enforced via `-D warnings` in CI. Common ones to watch
for when adding code:

- `Ok(x?)` → `x` (`needless_question_mark`)
- `&PathBuf` arg → `&std::path::Path` (`ptr_arg`)
- `matches!(_, Err(_))` → `.is_err()` (`redundant_pattern_matching`)
- `Error::new(ErrorKind::Other, e)` → `Error::other(e)` (`io_other_error`)
- `#[must_use]` on a function returning a `Result` (already `must_use`)
  → drop the attribute (`double_must_use`)

## What's deferred / stubbed

Called out in README.md + CHANGELOG.md too — keep them in sync if you
land any of these:

- **macOS keychain migration.** Explicitly skipped per project owner.
  Users on the legacy layout should `init` + `import` manually.
- **Cashu (ecash) wallet.** Staged for `--features cashu` using the
  `cdk` crate. Nothing in the active surface depends on it.
- **x402 listing-buy.** The public x402 facilitator the Python helper
  invokes isn't ready yet. The unrelated `buy` MCP tool (one-shot
  merchant-credit purchase + USDC send) is fully wired.

## Release flow

- Tag pattern `owallet-v*` fires `.github/workflows/owallet-release.yml`
  which builds the static matrix and attaches everything (+ SHA256SUMS)
  to a GitHub release. (Bare `v*` tags belong to the opencode fork at
  the repo root.)
- **Pre-public-release:** the workflow builds with `--features dev-envs`
  so the bundled binaries accept `--dev`/`--staging` and carry the
  baked-in staging Overpay URL (`defaults::OVERPAY_RAILS_URL_STAGING`)
  — norm defaults to staging until launch. Drop the feature flag from
  both build steps (and flip norm's `DEFAULT_ENV`) at the public
  release.
- Update `CHANGELOG.md` + bump `[workspace.package].version` in
  `Cargo.toml` + run `cargo build` so `Cargo.lock` updates, all in the
  same commit, before tagging.
- The `python_v0_1_0.db` filename does NOT track the Rust port's
  version (see "Test gotchas" above).

## Environment notes

This was developed in the Claude Code on-the-web sandbox. The container
has Python 3.11 + `cryptography` installable via pip, which is what the
DB fixture generator needs. Cargo / rustc 1.94 ships with the image.
Network access goes through the env's policy — crates.io and
github.com both reachable; new third-party services may need a policy
update.

### HTTPS calls from the binary fail in the sandbox (not an owallet bug)

Every outbound HTTPS request the binary makes in this sandbox fails with
`http transport: error sending request`. The sandbox intercepts outbound
TLS with its own inspection CA (`O=Anthropic; CN=sandbox-egress-production`).
`curl` works because it trusts that CA from the system cert store, but
owallet's `reqwest` is built with `rustls-tls` + bundled `webpki-roots`,
which don't include the sandbox CA — so it rejects the handshake. This is
a sandbox-only quirk and won't happen on a real machine.

To exercise something that needs a live HTTPS response (e.g. `list
marketplace`), fetch the real JSON with `curl` and serve it back over
plain local HTTP.
