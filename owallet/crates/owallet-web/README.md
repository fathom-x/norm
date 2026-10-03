# owallet-web

owallet compiled to `wasm32-unknown-unknown`: the `/health`, `/v1/*`
(OpenAI-compatible provider) and `/mcp` routers norm talks to, plus a
`/_mgmt/*` JSON API replacing the CLI verbs norm's bootstrap runs natively —
answered in-process through a fetch-shaped `handle(Request) → Promise<Response>`.
No sockets, no server: the browser's `fetch` reaches Overpay, SQLite runs on
`sqlite-wasm-rs` (OPFS or memory).

## Why a separate workspace

rusqlite gained its wasm32 backend in 0.38, while the main workspace is pinned
to 0.37 by `zcash_client_sqlite`, and `libsqlite3-sys` (`links = "sqlite3"`)
allows one version per lockfile. So this crate is its own workspace
(`[workspace]` in `Cargo.toml`, excluded from `owallet/Cargo.toml`) with its own
`Cargo.lock`; `owallet-db` asks for `rusqlite >=0.37, <0.39` and each lockfile
resolves what fits. The zcash/EVM stack never enters this graph: owallet-mcp is
built with `default-features = false` (no `evm`/`zcash`), so on-chain tools
answer `unavailable_in_browser`.

## Build

From this directory (`owallet/.cargo/config.toml` supplies the wasm settings:
clang as the C compiler for secp256k1/SQLite, getrandom's JS backend, and the
test runner):

```bash
cargo build --target wasm32-unknown-unknown --release
wasm-bindgen --target web --out-dir pkg \
  target/wasm32-unknown-unknown/release/owallet_web.wasm
cargo clippy --target wasm32-unknown-unknown --all-targets -- -D warnings
cargo clippy --all-targets -- -D warnings      # the native build of the same router
```

Needs `clang` + `llvm-ar`, and `wasm-bindgen-cli` at the `wasm-bindgen` version
in `Cargo.lock` (`cargo install wasm-bindgen-cli --version <it> --locked`).

## Test

Both suites run against the mock Overpay in `tests/mock-overpay/server.mjs`
(Node 22 stdlib only; `PORT`, `MOCK_STREAM_POLLS`; CORS on every response;
`GET /__mock/state`, `POST /__mock/reset`).

```bash
# Native: the same router, no browser (spawns the mock itself).
cargo test

# Browser: headless Chrome via chromedriver (versions must match).
PORT=4010 MOCK_STREAM_POLLS=2 node tests/mock-overpay/server.mjs &
CHROMEDRIVER=/path/to/chromedriver \
WASM_BINDGEN_TEST_WEBDRIVER_JSON=/path/to/webdriver.json \
  cargo test --target wasm32-unknown-unknown
```

`tests/browser.rs` runs on the main thread with memory storage (health,
init → generate → provider-key → `/v1/status`, `/v1/models`, streamed and
buffered chat completions, `/mcp` `tools/list` and an SSE `tools/call`, and the
Python `python_v0_1_0.db` fixture unlocking through the wasm SQLite).
`tests/opfs.rs` runs in a dedicated worker and checks OPFS persistence. The mock
URL is baked in at build time from `MOCK_OVERPAY_URL` (default
`http://127.0.0.1:4010`). `webdriver.json` holds the Chrome flags; point
`WASM_BINDGEN_TEST_WEBDRIVER_JSON` at a copy with
`"goog:chromeOptions": {"binary": …}` when Chrome is not on a standard path.

## JS API

```js
import initWasm, { init, handle, importMemoryDb } from "./pkg/owallet_web.js";
await initWasm();
await init({ rails_url: "https://overpay.com", env: "prod", storage: "opfs" });
const res = await handle(new Request("http://owallet.internal/_mgmt/status"));
```

- `init(config: OwalletWebConfig): Promise<void>` — `rails_url`, `public_url`,
  `env` (default `prod`), `storage` (`"opfs"` | `"memory"`, default memory;
  OPFS needs a dedicated worker), `db_name` (default `owallet.db`),
  `opfs_directory` (default `.owallet-web`). Calling it again reconfigures.
- `handle(req: Request): Promise<Response>` — any method/path; the URL's
  origin is ignored. Bodies stream (`ReadableStream`); cancelling the stream
  drops the Rust side.
- `importMemoryDb(name, bytes)` — preload a database file into the memory VFS.

### In norm's page

`packages/web-tui` builds this crate with `bun run build:owallet`
(`scripts/build-owallet-web.sh`: release wasm32 build + `wasm-bindgen --target
web` into `packages/web-tui/src/owallet-web/`, checking the toolchain and the
wasm-bindgen-cli version) and loads it lazily in its core worker
(`src/owallet.ts`) behind `http://owallet.internal`: OPFS storage
(`owallet.db` in the `.owallet-web` pool; memory where OPFS sync handles don't
exist), Overpay from the page's `?overpay=<url>` (default: norm's staging
Overpay). `bun run test:owallet` drives it end to end against
`tests/mock-overpay/server.mjs`. See that package's README.

`/_mgmt` routes (JSON in/out; errors are non-2xx
`{"error": {"code", "message"}}`): `GET status`, `POST init {password}`,
`POST unlock {password}`, `POST generate {wallet_password?, words?}`,
`POST import {mnemonic, wallet_password?}`, `POST select {npub}`,
`POST provider-key/create {label, spend, budget_usd}`, `POST overpay/register`,
`POST overpay/pkce/start {redirect_uri}`, `POST overpay/pkce/finish {code, state}`,
`GET credits`. See `src/mgmt.rs` for the response shapes.
