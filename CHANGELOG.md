# Changelog

All notable changes to the Rust port of `owallet` are documented here.

## Unreleased

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
