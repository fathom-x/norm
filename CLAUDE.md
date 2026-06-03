# CLAUDE.md — owallet-rs

Operational notes for working in this workspace. Read alongside the
top-level `/home/user/overpay/CLAUDE.md` (which covers the Rails app +
bot_manager).

## What this is

Rust port of the Python `owallet/` package that lives next to this
directory. The on-disk encrypted DB (`~/.owallet.db`) is intentionally
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

Original Python source is at `/home/user/overpay/owallet/wallet_mcp/`.
Cross-references in code comments use `wallet_mcp/server.py:NNNN` style.

## Run commands

```bash
# MUST cd into the workspace; `cd /home/user/overpay` won't work — cargo
# can't find Cargo.toml from the parent.
cd /home/user/overpay/owallet-rs

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

- **OAuth AS host_key.** When a wallet runs `owallet authorize` the
  bearer token is stored under `(npub, host)` where `host` is the
  trimmed `OVERPAY_RAILS_URL`. When the local OAuth AS issues an MCP
  token, `host_key` in the MCP state is the **issuer URL**, not the
  Rails URL. The MCP tools look up the Overpay bearer under
  `(active_npub, host_key)`, so when wiring tests make sure the
  `write_token` host matches what `McpState::new` was given as
  `host_key`.

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
  owallet-db; owallet-http uses `time` (formatting) for the
  `YYYY-MM-DD HH:MM UTC` display.

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

- Tag pattern `owallet-rs-v*` fires
  `.github/workflows/owallet-rs-release.yml` which builds the static
  matrix and attaches everything (+ SHA256SUMS) to a GitHub release.
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
