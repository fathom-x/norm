# owallet (Rust)

`owallet` is a local MCP wallet server: it exposes a Model Context Protocol
endpoint AI agents can call, a password-protected web dashboard for
day-to-day wallet management, an OAuth 2.0 Authorization Server so MCP
clients can authenticate against it, and a thin CLI for everything else.
Native USDC send on Base (and a handful of other EVM chains) is built in.

This crate is a Rust port of the Python `owallet/` implementation that
lives next to it in this repository. The on-disk wallet database is
**byte-compatible** with the Python version's `~/.owallet.db` format, so
existing wallets keep working after the upgrade.

## Workspace layout

```
owallet-rs/
  crates/
    owallet-crypto/   AES-256-GCM, PBKDF2-SHA256, BIP-39/BIP-32, Nostr,
                      NIP-98 — every cryptographic primitive used elsewhere
    owallet-db/       Encrypted SQLite layer; same schema + crypto as the
                      Python wallet_mcp.db
    owallet-config/   `.owallet` dotenv resolution (--prod/--dev/--staging),
                      multi-config support
    owallet-overpay/  Async REST client for the Overpay Rails API, plus
                      OAuth 2.0 PKCE helpers
    owallet-evm/      ERC-20 USDC transfer + balance via alloy
    owallet-mcp/      Hand-rolled JSON-RPC 2.0 MCP transport + tool registry
                      (the tools mirror wallet_mcp/server.py, incl. the
                      local purchase-cache tools list/get/sync_purchases)
    owallet-http/     axum router: dashboard + OAuth Authorization Server
                      + `/mcp` mount
    owallet/          The `owallet` binary itself (clap CLI)
```

## Install

Per-target static binaries are produced by the GitHub Actions release
workflow on every `v*` tag. Pick the artifact for your platform:

```bash
# macOS / Linux — extract and drop into your PATH
curl -L https://github.com/fathom-x/overpay/releases/download/<TAG>/owallet-<TARGET>.tar.gz \
  | tar -xz -C /usr/local/bin
```

Or build from source:

```bash
cd owallet-rs
cargo install --path crates/owallet
```

## Quick start

```bash
# create the encrypted wallet DB at ~/.owallet.db (prompts for a master password)
owallet init

# mint a fresh BIP-39 seed; prints the address and npub once
owallet generate

# (optional) link the wallet to your Overpay account via OAuth
owallet authorize

# run the dashboard + MCP server on http://127.0.0.1:8765
owallet serve

# install the local MCP server + the hosted Overpay MCP into Claude Code
owallet install --claude-local
```

The dashboard lives at `http://127.0.0.1:8765/wallet`. The MCP endpoint
is `http://127.0.0.1:8765/mcp` (JSON-RPC 2.0).

## Multi-env

`.owallet` config files (`prod.owallet`, `dev.owallet`, `staging.owallet`)
hold per-env overrides for `OVERPAY_RAILS_URL`, `OVERPAY_PUBLIC_URL`,
`OWALLET_PORT`. Combine flags to run multiple servers at once:

```bash
owallet --dev --staging serve            # one server per env, each on its OWALLET_PORT
owallet --prod --dev --staging serve --port 9001,9002,9003   # positional port overrides
owallet --staging install --claude-global # registers owallet-staging
```

To override a URL from the environment (rather than a `.owallet` file),
the var must carry the env's suffix: `OVERPAY_RAILS_URL_PROD`,
`OVERPAY_RAILS_URL_DEV`, `OVERPAY_RAILS_URL_STAGING` (same for
`OVERPAY_PUBLIC_URL`). A bare, unsuffixed `OVERPAY_RAILS_URL` from the
shell is deliberately ignored when a config is active — values come from
the config file or the suffixed env var. `owallet install` / `config
--mcp` register only the local `owallet` server (owallet no longer
calls a hosted Overpay MCP).

## CLI reference

| Command | What it does |
|---|---|
| `init` | Create the encrypted DB |
| `serve [--port LIST] [--host IP]` | Start the dashboard + OAuth AS + MCP |
| `generate [--words 12\|24]` | Mint a fresh BIP-39 mnemonic + key (default 24; prompts for a per-wallet web-admin password) |
| `import [--mnemonic\|--private-key]` | Bring an existing seed in |
| `select [WALLET]` | Set the default wallet (interactive without arg) |
| `export key [--format hex\|hex0x\|mnemonic] [--npub …]` | Print key material |
| `account` | Show wallet metadata + linked Overpay account (live) |
| `authorize` | Run OAuth PKCE against Overpay; store the bearer token |
| `login` | Open a one-time Overpay web session using the stored token |
| `list marketplace [--category --seller --cursor --limit]` | Browse listings |
| `send --to ADDR --amount USDC` | Sign + broadcast an ERC-20 USDC transfer |
| `install --{claude,opencode,codex}-{local,global}` | Register MCP entries |
| `config [--mcp]` | Show env config (or print the `.mcp.json` blob) |

Every command accepts `--config PATH` (explicit `.owallet` file, must exist)
or `--prod`/`--dev`/`--staging` (load the matching file from cwd; missing
is silently OK except for `--config`).

## Environment variables

| Var | Default | Used by |
|---|---|---|
| `OWALLET_DB_PATH` | `~/.owallet.db` | every command |
| `OWALLET_PASSWORD` | _(prompted)_ | non-interactive DB unlock |
| `OWALLET_WALLET_PASSWORD` | _(prompted)_ | non-interactive per-wallet password for `generate` / `import` |
| `OWALLET_PORT` | `8765` | `serve`, `install`, `config` |
| `OWALLET_HOST` | `127.0.0.1` | `serve` |
| `OWALLET_MCP_BASE_URL` | derived from bind addr | OAuth issuer URL |
| `OVERPAY_RAILS_URL[_<ENV>]` | `https://overpay.com` | `account`, `authorize`, `list marketplace`, MCP tools (suffixed form required as an env override when a config is active) |
| `OVERPAY_PUBLIC_URL[_<ENV>]` | = `OVERPAY_RAILS_URL` | browser-targeted URL rewriting |
| `EVM_RPC_URL` | `https://mainnet.base.org` | `send`, MCP `send_usdc` |
| `EVM_NETWORK` | `eip155:8453` (Base mainnet) | chain table lookup |

## Architecture notes

**Auth model.** `owallet`'s MCP transport accepts a bearer token issued by
the built-in OAuth AS at `/oauth/token` (PKCE S256). The AS is backed by
the encrypted DB (`oauth_clients`, `auth_codes`, `access_tokens` tables)
and the `/consent` page requires a per-wallet password before issuing.

**Overpay auth fallback.** When the MCP tool stack needs to call the
Overpay Rails API, it picks a stored Bearer token if one exists. If not,
it falls back to a per-request **NIP-98** envelope signed with the
wallet's secp256k1 key — so most Overpay calls work even without running
`owallet authorize` first.

**USDC send.** `send_usdc` (CLI + MCP tool) goes through `alloy` with
`with_recommended_fillers`, which auto-populates nonce / gas / EIP-1559
fees. The supported chains are Ethereum, Base, Base Sepolia, Optimism,
Polygon, and Arbitrum One — the USDC contract address and decimals per
chain are hard-coded in `owallet-evm::chains`.

## Migrating from the Python implementation

The on-disk encrypted DB (`~/.owallet.db`) is byte-compatible: PBKDF2-SHA256
with 600,000 iterations for the AES key, AES-256-GCM with 16-byte nonces
and tag appended to ciphertext, identical schema and migrations. Just
install the Rust binary and run any command — the existing wallets keep
working with the same master password.

### Not directly supported

The Python `owallet migrate` command also pulled credentials from the
macOS keychain and from `~/.wallet_tokens.json` (a legacy install layout
used before the encrypted DB existed). **The Rust port intentionally does
not re-implement that import path.** If you're upgrading from a build
predating the encrypted-DB layout, the recommended path is:

1. Install the Rust binary.
2. `owallet init` to create a fresh encrypted DB.
3. `owallet import --mnemonic "your seed phrase"` (or `--private-key 0x…`).
4. `owallet authorize` to relink the wallet to Overpay.

Once your seed is in the encrypted DB, every future binary upgrade is
silent — no migration step required.

## Status of upstream-Python features

The Rust port covers everything in the Python `wallet_mcp` package
except the three explicit defer cases below.

- **x402 listing-buy flow**: the public `buy_listing_with_x402` Python
  helper invokes the Overpay x402 facilitator. The facilitator isn't
  public yet so the Rust port doesn't include x402 transitively. The
  separate `buy` MCP tool (one-shot merchant-credit purchase + on-chain
  USDC send) **is** wired up.
- **Cashu (ecash) wallet**: not in this build. The Python implementation
  carried the Cashu wallet as an optional extra. The Rust port is staged
  to add it back behind a `--features cashu` cargo feature, using the
  `cdk` crate; nothing in the active surface needs it.
- **macOS keychain import**: not supported — see the migration section above.

## Testing

Every crate has its own tests; run the workspace suite:

```bash
cargo test --workspace
```

Notable test pieces:

- `owallet-db/tests/python_compat.rs` opens a checked-in
  `python_v0_1_0.db` fixture (generated by Python with the same crypto
  parameters) and asserts the Rust port unlocks + decrypts it cleanly.
- `owallet-overpay/tests/client_test.rs` drives every Rails endpoint
  through `wiremock`, including a regex assertion that `Authorization:
  Nostr <b64>` reaches the server when the NIP-98 fallback is in play.
- `owallet-evm/tests/send_test.rs` mocks the entire
  `eth_chainId → eth_sendRawTransaction → eth_getTransactionReceipt`
  cycle through alloy so the broadcast path is exercised without a
  live RPC node.
- `owallet-http/tests/mcp_test.rs` covers the MCP transport end-to-end
  via `axum_test::TestServer`, including the local OAuth AS metadata,
  bearer validation, and the NIP-98 fallback.

## License

MIT, matching the Python implementation.
