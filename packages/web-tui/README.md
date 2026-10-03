# @opencode-ai/web-tui — norm in the browser

The opencode core (sessions, tools, providers, the norm layer) running in a
dedicated **Web Worker**, answering the TUI's RPC protocol unchanged. Today the
page is a small debug panel over that worker; the real TUI (`packages/tui`)
takes over once opentui runs on WebAssembly (`packages/opentui-wasm`), and
owallet arrives as `owallet-web` behind `http://owallet.internal`.

```
page (main thread)                      core worker (dedicated, OPFS lives here)
  src/main.ts — debug panel              src/core.worker.ts
  src/core-client.ts                       1. shims/globals.ts (process, Buffer)
    Rpc.client + createWorkerFetch  ──►    2. fetch router: owallet.internal → owallet-web
      { fetch, reload, shutdown }          3. ZenFS VFS mounted (OPFS dir `norm-vfs`)
    ◄── global.event, ready, boot          4. import opencode/cli/tui/worker.browser.ts
                                              HttpApiApp.webHandler() + Rpc.listen
                                              sqlite-wasm in OPFS sahpool (`.norm-sqlite`)
```

## Commands

Run from `packages/web-tui` (never from the repo root).

| What | Command |
| --- | --- |
| Build page + worker into `dist/` | `bun run build` (`vite build`) |
| Serve the build | `bun run preview` → http://127.0.0.1:4173/ |
| Unit tests (bun + happy-dom, `--conditions=browser`) | `bun run test` |
| Browser smoke test (build, then headless Chromium) | `bun run test:browser` |
| Typecheck | `bun run typecheck` |

The one supported build path is **`vite build` + `vite preview`** (for
iteration, `vite build --watch` in one terminal and `vite preview` in
another). The `vite` dev server is not supported: its dependency
pre-bundling (esbuild) bypasses the Node shims below, so the worker fails to
start.

Page URL flags: `?mock-owallet` answers `owallet.internal` with the scripted
stand-in in `src/mock-owallet.ts` (no wallet needed; its "model" drives the
file tools), `?debug` prints the core's logs and norm's bootstrap diagnostics
to the devtools console. In the console, `norm.api("/session")` calls the
core server and `norm.core` is the RPC client.

`test/browser/smoke.mjs` reads `CHROME` (Chromium binary), `PLAYWRIGHT` (path
to playwright's `index.js`) and `PORT` (default 4317); screenshots go to
`test/screenshots/`. It checks: boot on OPFS with the demo workspace,
`POST /session` + `GET /session` through `Rpc`, a file read, persistence
across a reload, and scripted write → read, grep → glob, read → edit and bash turns through
the real session loop (including owallet's `charged_cents` as the cost).

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

## Plugging in the real TUI (M1)

`src/main.ts` becomes the TUI host once `@opentui/core` has its wasm backend
(`packages/opentui-wasm`). Mirror the worker branch of
`packages/opencode/src/cli/cmd/tui.ts`:

1. `const core = startCore()` (`src/core-client.ts`) and `await core.ready`.
2. Build the transport the native TUI uses for its Bun worker:
   `{ url: "http://opencode.internal", fetch: core.fetch, events: { subscribe: async (handler) => core.onEvent(handler) } }`
   (`core.fetch` is `createWorkerFetch`; `onEvent` listens for `global.event`).
3. Call `run` from `opencode/cli/tui/layer` (which wraps
   `@opencode-ai/tui`'s `run`, `packages/tui/src/app.tsx`) with that
   transport, `directory: "/workspace"`, the TUI config,
   `pluginHost: createLegacyTuiPluginHost()` and no `onSnapshot`, rendering
   into the xterm.js-backed renderer.
4. The TUI's own main-thread file access (`util/persistence.ts`,
   `sidebar/owallet.tsx`'s `node:fs/promises` + `Global`) needs the same
   aliases as the worker, but must not open a second ZenFS over the same OPFS
   directory: mount the worker's tree on the main thread with ZenFS's `Port`
   backend instead.
5. The TUI calls owallet directly (the sidebar's `/v1/status`): route
   `owallet.internal` on the main thread to the worker — add an RPC method
   that runs the worker's `fetch` (which the router there answers) — once
   owallet-web is registered in `core.worker.ts` in place of
   `owalletUnavailable`.

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
