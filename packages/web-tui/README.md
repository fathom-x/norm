# @opencode-ai/web-tui — norm in the browser

The real norm TUI (`packages/tui`) in a browser tab: xterm.js plus opentui on
its WebAssembly core (`packages/opentui-wasm`) on the page, and the opencode
core (sessions, tools, providers, the norm layer) in a dedicated **Web
Worker**, talking over the same RPC the native TUI uses for its Bun worker.
owallet is `owallet-web` behind `http://owallet.internal` in the worker.

```
page (main thread)                           core worker (dedicated; OPFS lives here)
  src/main.ts                                  src/core.worker.ts
    startCore() ─────── Rpc (JSON) ─────►        1. shims/globals.ts (process, Buffer)
    await core.ready                             2. fetch router: owallet.internal → owallet-web
    // setup screen: wired by the lead           3. ZenFS VFS mounted (OPFS dir `norm-vfs`),
    src/tui.ts                                      served to the page (serveVfs)
      xterm.js + FitAddon                        4. opencode/cli/tui/worker.browser.ts
      bootOpenTUIWasm (opentui-wasm)                HttpApiApp.webHandler() + Rpc.listen,
      process/Bun/timers completed                  + worker-rpc.ts: privateFetch
      ZenFS Port mount of the worker's tree ◄── BroadcastChannel ──► attachFS
      owallet.internal → core.privateFetch  ──► worker's router
    src/tui-run.ts
      TuiConfig.get(), run() (opencode/cli/tui/layer),
      transport { url: opencode.internal, fetch: core.fetch, events: global.event }
```

## Commands

Run from `packages/web-tui` (never from the repo root).

| What | Command |
| --- | --- |
| Build page + worker into `dist/` | `bun run build` (`vite build`; needs `packages/opentui-wasm/dist`, see below) |
| Serve the build | `bun run preview` → http://127.0.0.1:4173/ |
| Unit tests (bun + happy-dom, `--conditions=browser`) | `bun run test` |
| TUI browser test (build, then headless Chromium) | `bun run test:tui` |
| Core smoke test through the debug panel | `bun run test:browser` |
| owallet-web in the worker, against the mock Overpay | `bun run test:owallet` |
| End to end: setup screen → TUI → owallet-web → mock Overpay, then unlock and a second tab | `bun run test:e2e` |
| Typecheck | `bun run typecheck` |

`test:owallet` and `test:e2e` build owallet-web first (`bun run build:owallet`:
Rust with the wasm32 target, clang + llvm-ar, and the matching
wasm-bindgen-cli). The mock Overpay is
`owallet/crates/owallet-web/tests/mock-overpay/server.mjs`.

Serving from a sub-path (e.g. GitHub Pages, `https://<owner>.github.io/norm/`):
build with `NORM_WEB_BASE=/norm/ bun run build`; assets, workers and the OAuth
callback page resolve against it. `.github/workflows/norm-web-pages.yml`
(manual dispatch) builds and deploys exactly that; its header lists what the
target Overpay must allow (`API_CORS_ORIGINS`, `OAUTH_ALLOWED_REDIRECT_URIS`,
optionally `DEMO_CREDITS_CENTS`).

CI: `.github/workflows/norm-web-ci.yml` runs all of the above (plus the
opentui wasm build and norm's TUI suite on the wasm core). Its
`staging-smoke` job runs `e2e.mjs` against a real Overpay nightly and on
demand when `E2E_OVERPAY_URL` (repository variable
`NORM_WEB_E2E_OVERPAY_URL`) and `E2E_MNEMONIC` (secret
`NORM_WEB_E2E_MNEMONIC`, a wallet whose account holds core credits) are set;
that Overpay's `API_CORS_ORIGINS` must include `http://127.0.0.1:4320`.

The page needs opentui's wasm build: `bash ../opentui-wasm/scripts/build.sh`
once (it writes `packages/opentui-wasm/dist/`: `opentui.wasm`, `core/`,
`solid/`).

The one supported build path is **`vite build` + `vite preview`** (for
iteration, `vite build --watch` in one terminal and `vite preview` in
another). The `vite` dev server is not supported: its dependency
pre-bundling (esbuild) bypasses the Node shims below. Several agents/CI jobs
building at once: `vite build --outDir <dir>` and pass `DIST=<dir>` to the
browser tests.

Page URL flags:

| Flag | Effect |
| --- | --- |
| `?mock-owallet` | `owallet.internal` is the scripted stand-in in `src/mock-owallet.ts` (no wallet needed; its "model" drives the file tools by keyword; `/v1/status` and an empty `/mcp` for the sidebar) |
| `?debug` | the core's logs and norm's bootstrap diagnostics in the devtools console |
| `?debug-panel` | the plain-DOM debug panel instead of the TUI (`norm.api("/session")` in the console) |
| `?overpay=<url>` | the Overpay owallet-web talks to — only a known deployment (`src/overpay-target.ts`: overpay.com, the staging Overpay, `NORM_WEB_OVERPAY_URLS` at build time), or a loopback Overpay from a loopback page; anything else is ignored, so a link cannot point the tab's wallet at a look-alike marketplace |
| `?session=<id>` | open a session (the CLI's `--session`). There is deliberately no `?prompt=`: the TUI submits a startup prompt as the user's own message, so a link could spend the visitor's wallet |

`window.__norm = { term, core, host }` is there for tests and the console.

Browser tests read `CHROME` (Chromium binary), `PLAYWRIGHT` (path to
playwright's `index.js`), `PORT` and `DIST`; screenshots go to
`test/screenshots/`.

- `test/browser/tui.mjs` (port 4319, `tui-*.png`): the home screen renders in
  xterm (logo, prompt placeholder); typing a prompt + Enter creates a session
  and the scripted reply (write → read through the real tools) appears; the
  sidebar shows owallet's status from `/v1/status`; ctrl+p opens the command
  palette; the grid follows the viewport size; after a reload ctrl+x l lists
  the session. Any page error or console error fails it (allowlist in the
  file, empty today).
- `test/browser/smoke.mjs` (port 4317, `0*-*.png`, via `?debug-panel`): boot
  on OPFS with the demo workspace, `POST /session` + `GET /session` through
  `Rpc`, a file read, persistence across a reload, and scripted
  write → read, grep → glob, read → edit and bash turns through the real
  session loop (with owallet's `charged_cents` as the cost).

## The page (main thread)

`vite.config.ts` builds the page with `browserBuild({ thread: "main" })` and
`vite-plugin-solid` in opentui's universal mode
(`{ generate: "universal", moduleName: "@opentui/solid" }`); the worker with
`browserBuild({ thread: "worker" })`. One alias table, in
`build/browser-build.ts`:

| Specifier | Worker | Page |
| --- | --- | --- |
| `fs`, `fs/promises`, `path`, `os`, `url`, `util`, `crypto`, `async_hooks`, `timers/promises`, `module`, `diagnostics_channel`, `events`, `buffer`, `stream`, `@effect/platform-node*` | `src/shims` / npm polyfills (below) | same — `fs` is ZenFS, on the page the worker's tree over a port |
| `process` | `src/shims/process.ts` | the same module, which on the page returns opentui-wasm's process (installed by `bootOpenTUIWasm`, stdin/stdout wired to xterm) after `completeProcess` gave it env, `cwd()` = `/workspace`, … |
| `perf_hooks`, `console`, `worker_threads`, `tty`, `child_process` | throwing stubs | opentui-wasm's shims (`packages/opentui-wasm/src/shims`) |
| `bun` | stub | `src/shims/bun.ts` (`Bun.file().text/json`, `Bun.write`, `Bun.stringWidth`, file-URL helpers; also installed as the `Bun` global) |
| `open`, `clipboardy` | `window.open` / `navigator.clipboard` (best effort) | same |
| `@opentui/core[/x]`, `@opentui/solid[/x]` | — | `packages/opentui-wasm/dist`, by the package's `exports` (`browser` first) |
| `@opentui/core/testing` | — | opentui-wasm's throwing stub |
| `@opentui/solid/runtime-plugin-support[/configure]` | — | no-op (`src/shims/runtime-plugin-support.ts`): no runtime loading of external TUI plugins in a tab |
| bare imports from inside `opentui-wasm/dist` (`solid-js/…`, `entities`) | — | resolved from this package: one solid-js on the page (`resolve.dedupe` too) |
| `@opentui/core-<platform>` native libraries | — | absent |

Two edits to the built opentui core (`patchOpentuiDist`): `CliRenderer`
installs its own `requestAnimationFrame` on the global object, which in a
browser is `window` — it captured xterm.js's frames and the terminal's DOM
stopped updating, so the override is renamed away (the TUI's few callers use
the browser's rAF); and the tree-sitter loader's
`` new URL(`./${path}`, import.meta.url) `` would make Vite copy all of
`dist/core` into the build, so it is left to runtime.

Boot order (`src/main.ts` → `src/tui.ts` → `src/tui-run.ts`) matters:
`main.ts` keeps a placeholder `process.env` until opentui-wasm installs the
real process; `tui.ts` creates the terminal (FitAddon, ResizeObserver,
focus), boots the wasm core, completes the process, installs `Bun` and
Node-style timer handles (`.unref()`, `.refresh()`; `src/shims/timers.ts`),
mounts the worker's file tree (`src/main-vfs.ts`) and routes
`owallet.internal` to the worker; only then is the TUI imported.

The page's files are the worker's: ZenFS's `Port` backend over a per-page
`BroadcastChannel`. Async calls go to the worker; sync calls (`existsSync`,
`realpathSync`, used by the TUI only for path checks) read a cache filled when
the page mounts, so they see the page's own writes but not files the worker
creates later. `/tmp` is private to each side.

### Setup screen seam

`src/main.ts`, between `await core.ready` and the TUI import:

```ts
await core.ready
// setup screen: wired by the lead
const { startTui } = await import("./tui")
```

### Known gaps

- No tree-sitter syntax highlighting (parser worker and grammars are not
  served yet), no audio (compiled out of the wasm core), no kitty keyboard
  protocol (xterm.js; opentui falls back to legacy keys), 256 MiB fixed wasm
  memory for the renderer.
- External TUI plugins (npm or `.opencode/plugin` files) are not loaded; the
  built-in ones run.
- Clipboard: copy uses `navigator.clipboard` (needs focus/permission); OSC 52
  is ignored by xterm.js.
- Main-thread sync fs reads can be stale for files the worker wrote after the
  page mounted (see above).
- One tab at a time: the OPFS stores (`norm-vfs`, `.norm-sqlite`,
  `.owallet-web`) are exclusive.

## How the core is made to bundle (`build/browser-build.ts`)

1. **Browser twins** — whole modules swapped for a sibling with the same
   exports, where the module *is* the platform seam:

   | Native | Browser | Why |
   | --- | --- | --- |
   | `core/src/effect/app-node-platform.ts` | `app-browser-platform.ts` | FileSystem = ZenFS (`vfs-filesystem.ts`), Path = Effect's POSIX, HTTP = fetch |
   | `core/src/cross-spawn-spawner.ts` | `cross-spawn-spawner.browser.ts` | every spawn fails `NotFound`, like a missing binary |
   | `core/src/ripgrep.ts` | `ripgrep.browser.ts` | find/glob/grep in JS over the VFS (no rg binary) |
   | `core/src/npm.ts` | `npm.browser.ts` | no package manager: config installs no-op, plugin installs fail |
   | `opencode/src/tool/shell.ts` | `shell.browser.ts` | `bash` explains it can't run and points at `run_python` |
   | `opencode/src/tool/webfetch.txt` | `webfetch.browser.txt` | description notes the CORS limit |

   Plus the `browser` import condition in `packages/core/package.json`:
   `#sqlite` → `sqlite.browser.ts` (sqlite-wasm OO1 + drizzle sqlite-proxy),
   `#pty` → `pty.browser.ts` (refuses), `#fff` → `fff.browser.ts` (always
   unavailable, so `FileSystemSearch` falls back to the JS ripgrep twin with
   fuzzysort). The condition sits after `bun`/`node`, so Bun (even with
   `--conditions=browser`, as `bun dev` runs) and Node resolve as before.

2. **Node built-in shims** (`src/shims/`): `process` (env from `src/env.ts`,
   `cwd()` = `/workspace`), `os`, `path` (path-browserify), `fs` /
   `fs/promises` (ZenFS — the same tree as the Effect FileSystem),
   `url` (file-URL helpers; `fileURLToPath` of an http URL returns its path),
   `util`, `crypto` (randomness + @noble/hashes digests/HMAC),
   `async_hooks` (synchronous-scope AsyncLocalStorage), `timers/promises`,
   `tty`, `module`, `diagnostics_channel`, `events` / `buffer` / `stream`
   (npm polyfills), and `@effect/platform-node` (+ `/NodeFileSystem`,
   `/NodePath`) → the VFS and POSIX path layers. A name a shim lacks links to
   a throwing stub.
3. **Stubs**: every other built-in (`http`, `net`, `child_process`, `zlib`,
   `worker_threads`, …) and `bun` / `bun:*` become modules whose functions
   throw "`<name>` is not available in the browser build" when *called*.
   Each such call sits on a path the browser build turns off.
4. **Absent modules** link but throw when evaluated, so only the lazy
   `import()` that reaches them rejects (and every one is already handled):
   `opencode-web-ui.gen.ts`, `@silvia-odwyer/photon-node` (image resizing
   reports itself unavailable), `@aws-sdk/credential-providers`,
   `google-auth-library`.
5. **Inert auth plugins**: `opencode-gitlab-auth` and `opencode-poe-auth`
   start OAuth callback servers at import; norm offers only Overpay.
6. `*.md` / `*.txt` imports load as text (`?raw`); Bun's
   `import x from "a.wasm" with { type: "file" }` becomes the asset URL;
   `process.env` stays live (Vite would inline `{}`).

## What is off in the browser, and how

| Feature | State | Mechanism |
| --- | --- | --- |
| bash, PTYs | refused with a clear message | `shell.browser.ts`, `pty.browser.ts` |
| git / snapshots / VCS | no VCS: the project is "global" | spawner twin; `project.ts` already catches failing git |
| LSP, formatters | disabled / degrade | spawner twin, `OPENCODE_DISABLE_LSP_DOWNLOAD` |
| ripgrep binary download | not needed | `ripgrep.browser.ts` |
| file watcher (@parcel/watcher) | off | `OPENCODE_EXPERIMENTAL_DISABLE_FILEWATCHER` |
| runtime plugin / npm installs | off | `npm.browser.ts` |
| local stdio MCP servers | fail to start | spawner twin |
| MCP OAuth callback server | never loaded | norm's browser defaults set `mcp.owallet.oauth = false` (`NORM_RUNTIME=browser`) |
| models.dev catalog | not fetched | `OPENCODE_DISABLE_MODELS_FETCH` — it *is* CORS-readable, but norm's models come from owallet's `/v1/models` |
| sharing | off | `OPENCODE_DISABLE_SHARE` |
| image resizing (photon) | unavailable | absent module |
| webfetch | works for CORS-enabled sites only | browser `fetch` |
| AsyncLocalStorage | synchronous scope only | `async-hooks.ts`; `WorkspaceContext` tolerates it |

Storage: ZenFS mounts OPFS directory `norm-vfs` at `/` (in-memory `/tmp`);
norm's state lives under `NORM_HOME=/norm`, the demo project under
`/workspace` (written once, never overwritten). SQLite uses its own OPFS
sahpool directory `.norm-sqlite`; both are exclusive to one tab at a time.
Outside a worker with OPFS (tests) both fall back to memory.

## owallet-web in the page

The real owallet — `owallet/crates/owallet-web`, Rust compiled to
`wasm32-unknown-unknown` — answers `http://owallet.internal` in the core worker
(`src/owallet.ts`, registered in `core.worker.ts` unless `?mock-owallet`).

| What | Command |
| --- | --- |
| Build the module into `src/owallet-web/` (generated, gitignored) | `bun run build:owallet` (`scripts/build-owallet-web.sh`) |
| Then the page | `bun run build` |
| Browser test against the mock Overpay | `bun run test:owallet` |

`build:owallet` is not part of `build` (a cold release build of the Rust crate
takes minutes and needs Rust + the wasm32 target, clang/llvm-ar and
`wasm-bindgen-cli` at the version in owallet-web's `Cargo.lock`; the script
checks each and says what is missing). Without it the page still builds — the
module is found through `import.meta.glob` — and owallet.internal answers 503
`owallet_unavailable` saying to run it.

- **Lazy**: the ~7 MB module loads on the first owallet request, which awaits
  it; a failed or stuck start (30 s, e.g. the wallet open in another tab)
  answers 503 JSON with the reason and is retried on the next request.
- **Config** (`WorkerOptions.overpay = { railsUrl, env?, publicUrl? }`, from
  the page's `?overpay=<url>`): defaults to norm's staging Overpay
  (`https://overpay-eykm.onrender.com`, env `staging`). At runtime,
  `configureOwallet(worker, overpay)` (an `owallet.configure` message) re-points
  it; the wallet database is kept.
- **Storage**: the wallet DB `owallet.db` in its own OPFS access-handle pool
  (`.owallet-web`, separate from the core's `.norm-sqlite`), or memory where
  OPFS sync handles don't exist. The password is never stored: after a reload
  `/_mgmt/status` reports `initialized` + locked until `/_mgmt/unlock`.
- **Overpay** is called cross-origin from the worker: it must list the page's
  origin in `API_CORS_ORIGINS` (overpay's `config/initializers/cors.rb`). The
  mock (`owallet/crates/owallet-web/tests/mock-overpay/server.mjs`) allows any.

`test/browser/owallet.mjs` (env as smoke.mjs, `PORT` default 4318) starts the
mock on a random port, opens `/?debug-panel&overpay=…`, and checks: `/_mgmt`
init → generate → overpay/register through the worker's router; norm's
bootstrap minting an `owk_` key into `auth.json`; `/v1/models`; a prompt
streaming the mock seller's reply with the turn's cost = `charged_cents`; the
owallet MCP server connected with its tools; and after a reload the wallet
persisted but locked, then unlocked. Screenshots: `04-owallet-web-chat.png`,
`05-owallet-web-unlocked.png`.

## Security notes

- **owallet's management API is gated.** Every HTTP client in the core worker
  reaches `http://owallet.internal` through the fetch router — including the
  model's `webfetch` and any provider or MCP URL the model could write into a
  config file. `/_mgmt` (create/unlock the wallet, mint keys, link Overpay)
  therefore needs a per-boot capability header (`src/mgmt-gate.ts`) that only
  the page (`worker-rpc.ts` `privateFetch`) and norm's wasm host
  (`packages/opencode/src/norm/host.ts` `mgmtHeaders`) present; `/mcp` and `/v1`
  need a provider key (no anonymous `/mcp` in the browser), and `/_mgmt`
  bodies with unknown fields are refused.
- **No `?prompt=`**: the TUI submits a startup prompt as the user's own
  message, so a link could spend the visitor's wallet.
- **Overpay login popup**: opened with `noopener`; the callback page
  (`public/oauth/callback.html`) hands the code back over a same-origin
  `BroadcastChannel` and the setup screen checks `state`. Overpay must list the
  callback's exact URL in `OAUTH_ALLOWED_REDIRECT_URIS`.
- **Origin**: everything the page keeps (OPFS: wallet, `auth.json`, sessions)
  is per origin. Serve it from an origin of its own — not a shared
  `https://<owner>.github.io` that other sites of the same owner also use.
- **Aborts**: a request's signal reaches owallet-web (`src/abort.ts`): a
  cancelled streamed reply drops the Rust stream, as a closed connection does
  natively.

