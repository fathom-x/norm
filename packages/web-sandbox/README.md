# @opencode-ai/web-sandbox — norm in a cloud sandbox

The demo service for "variant B": the **native** `norm` CLI (with `owallet`,
bash and all) runs in a private per-visitor sandbox, and only its terminal is
streamed to an xterm.js page. The same service also serves the landing page
that offers both demos, and optionally variant A (`packages/web-tui`, norm
entirely in the browser) under `/browser/`.

```
browser /sandbox/ (xterm.js)
   │  WebSocket /api/tty: binary = terminal bytes both ways; text = JSON control
   ▼
broker (Bun, server/)  ── holds E2B_API_KEY and SESSION_SECRET; signed `sid` cookie
   │  SandboxProvider
   ├─ e2b:   one E2B sandbox per visitor (template "norm-demo"), paused between
   │         visits; PTY → norm-demo → norm → owallet; egress: Overpay only
   └─ local: a PTY on this machine (bun-pty) per visitor — development and tests
```

## Layout

| Path | What |
| --- | --- |
| `server/main.ts` | entry point (`bun run start`): config from env, `createServer`, signal handling |
| `server/server.ts` | `createServer(config, {provider?})`: routes, static files, the Bun WebSocket |
| `server/hub.ts` | the terminal protocol: hello, phases, one connection per visitor, takeover, grace → pause; `admission` (caps) |
| `server/provider.ts` | the `SandboxProvider` interface |
| `server/providers/local.ts` | the local provider (bun-pty) |
| `server/providers/e2b.ts` | the E2B provider (injected SDK; `realSdk()` binds the `e2b` package) |
| `server/cookie.ts`, `request.ts`, `rate-limit.ts`, `static.ts`, `config.ts` | session cookie, origin/IP, token buckets, files, env |
| `web/` | Vite pages: `index.html` (landing), `sandbox/index.html` + `src/sandbox.ts` (the terminal) |
| `template/` | the E2B template `norm-demo` (see its own files) |
| `test/unit/` | `bun test` suites |
| `test/browser/local.mjs` | headless Chromium end to end with the local provider |

## Commands

Run from `packages/web-sandbox`.

| What | Command |
| --- | --- |
| Build the pages into `dist/web` | `bun run build` |
| Run the service | `bun run start` (→ http://127.0.0.1:4330/) |
| Build once, then run with restart-on-change | `bun run dev` (for page changes also run `bunx vite build --watch` in another terminal) |
| Unit tests | `bun run test` |
| Browser test (build, then headless Chromium) | `bun run test:browser` |
| The real norm through the broker (local provider, mock Overpay) | `bun run test:norm` |
| Build the E2B template | `bun run template:build` (see `template/README.md`) |
| Typecheck | `bun run typecheck` |

### Run it locally (local provider)

The local provider runs `SANDBOX_COMMAND` (default `norm-demo`, the
template's wrapper) in a PTY per visitor, with `NORM_HOME=<root>/<sid>/norm`,
the working directory `<root>/<sid>/workspace` and a random
`OWALLET_PASSWORD` kept in `<root>/<sid>/.owallet-password` (0600) — so norm's
first run sets up a wallet without a prompt. To try the real CLI with a
`norm` on your PATH:

```bash
bun run build
SANDBOX_COMMAND=norm bun run start
# open http://127.0.0.1:4330/sandbox/
```

Any program works, e.g. `SANDBOX_COMMAND=bash SANDBOX_ARGS='["-l"]'`. It is
**not** isolation: the program runs as you, with your filesystem and network.
Only the variables in `SANDBOX_PASS_ENV` (PATH, HOME, LANG, …) reach it, so
the broker's own secrets do not.

With variant A as well: build it (`cd ../web-tui && NORM_WEB_BASE=/browser/
bun run build`) and set `BROWSER_DIST=../web-tui/dist`.

### The browser test

`test/browser/local.mjs` starts the service (port `PORT`, default 4331) with
the local provider running a scripted bash program (READY + its pid, then a
counter per line, `size` → `stty size`) and checks: the landing page (both
cards; the browser card hidden without `BROWSER_DIST`; no horizontal scroll at
phone width) → `/sandbox/` shows READY → typing echoes → a viewport resize
reaches the PTY → a reload resumes the **same** process (same pid, the
counter keeps counting) → a second tab takes over and the first shows
"Open in another tab" → "Start over" gives a fresh process. Any page or
console error fails it. Env: `CHROME`, `PLAYWRIGHT`, `PORT`, `DIST` (as in
`packages/web-tui`). Screenshots: `test/screenshots/local-*.png`.

### The real-norm test

`test/browser/norm.mjs` (`bun run test:norm`) is the E2B flow minus E2B: the
local provider runs the template's own wrapper (`template/norm-demo.sh`) with
norm from source and native owallet (`cargo build -p owallet --features
dev-envs` in `owallet/`; `OWALLET_BIN` overrides) against the mock Overpay
(`owallet/crates/owallet-web/tests/mock-overpay/server.mjs`), where a new
account starts at $0 with demo credits on offer. It checks: norm's first run
asks how to connect (no password prompt) → new account, no login → demo
credits → the TUI → a prompt answered by the mock seller through owallet,
with its real charge in the sidebar → a reload resumes the same norm process.
Screenshots: `test/screenshots/norm-*.png`.

Not yet: the same flow on real E2B against staging Overpay — it needs
`E2B_API_KEY` and a built template.

## Deploy (one demo service)

The image serves the landing page, `/sandbox/` and `/browser/` (variant A,
built with `NORM_WEB_BASE=/browser/`) from one origin:

```bash
packages/web-sandbox/scripts/stage-demo.sh       # pages → .demo/ (needs the browser build's inputs)
docker build -f packages/web-sandbox/Dockerfile -t norm-demo .
docker run -p 8080:8080 -e E2B_API_KEY=… -e SESSION_SECRET=$(openssl rand -base64 48) norm-demo
```

`.github/workflows/norm-demo-image.yml` (manual) does all of that and pushes
`ghcr.io/<owner>/norm-demo`; `render.yaml` (repo root) deploys it as a Render
web service (needs a GHCR registry credential in Render, and `E2B_API_KEY`).
The sandboxes run the E2B template (`bun run template:build`, separately).

What the Overpay behind it must allow (staging by default): `API_CORS_ORIGINS`
gets the demo origin (variant A calls Overpay from the page);
`OAUTH_ALLOWED_REDIRECT_URIS` gets `https://<demo>/browser/oauth/callback.html`
(variant A's "use my existing account"); `DEMO_CREDITS_CENTS` set so new
visitors can try it without paying. Sandboxes reach Overpay server-to-server,
so they need no CORS — only `OVERPAY_HOSTS` (their egress allowlist).

## Environment

| Variable | Default | Meaning |
| --- | --- | --- |
| `HOST` | `127.0.0.1` | listen address (`0.0.0.0` in a container) |
| `PORT` | `4330` | listen port |
| `SANDBOX_PROVIDER` | `local` | `local` or `e2b` |
| `SESSION_SECRET` | random per process | HMAC key for the `sid` cookie, ≥ 32 chars. **Required** with `NODE_ENV=production` or `SANDBOX_PROVIDER=e2b` (else every restart forgets every visitor) |
| `COOKIE_SECURE` | off | always mark the cookie `Secure` (it is anyway on https requests) |
| `ALLOWED_ORIGINS` | none | extra origins (comma-separated) allowed to open `/api/tty` and POST `/api/reset`; the service's own origin is always allowed |
| `TRUST_PROXY` | `0` | proxy hops in front (`1`/`true` = one): take scheme, host and client IP from `X-Forwarded-*` (IP = the Nth entry from the right of `X-Forwarded-For`) |
| `WEB_DIST` | `dist/web` | the built pages |
| `BROWSER_DIST` | unset | variant A's build, served at `/browser/` (build it with `NORM_WEB_BASE=/browser/`); unset = no `/browser/`, and the landing page hides that card |
| `PAUSE_GRACE_MS` | `60000` | after the last connection closes, pause the sandbox this much later unless the visitor is back |
| `MAX_SANDBOXES` | `20` | refuse to create a sandbox while this many are running |
| `RETENTION_DAYS` | `7` | e2b: delete this app's paused sandboxes created more than this many days ago (`0` keeps them) |
| `SWEEP_INTERVAL_MS` | `3600000` | how often the retention sweeper runs (also once at start) |
| `CREATES_PER_IP_PER_HOUR` | `5` | sandbox creations per client IP (token bucket; resuming your own is free) |
| `HELLO_TIMEOUT_MS` | `10000` | a socket that sends no `hello` in time is closed |
| `NORM_OWALLET_ENV` | `staging` | passed to norm (`prod`/`dev`/`staging`) |
| `SANDBOX_COMMAND` | `norm-demo` | local: the program in the PTY |
| `SANDBOX_ARGS` | `[]` | local: its arguments, a JSON array |
| `SANDBOX_ENV` | `{}` | local: extra environment, a JSON object |
| `SANDBOX_PASS_ENV` | `PATH,LANG,LC_ALL,LC_CTYPE,TMPDIR,USER,LOGNAME,SHELL,HOME` | local: the broker's own variables passed through |
| `SANDBOX_ROOT` | `$TMPDIR/norm-web-sandbox` | local: per-visitor state lives in `<root>/<sid>/` |
| `SANDBOX_IDLE_KILL_MS` | `0` (never) | local: kill a visitor's PTY this long after it was paused |
| `E2B_API_KEY` | — | e2b: required |
| `E2B_TEMPLATE` | `norm-demo` | e2b: template name or id |
| `E2B_TIMEOUT_MS` | `900000` (15 min) | e2b: sandbox timeout; on timeout it **auto-pauses**. Typing extends it (at most every third of it); an idle tab lets it pause |
| `OVERPAY_HOSTS` | `overpay-eykm.onrender.com` | e2b: the egress allowlist (hostnames, IPs or CIDRs); everything else is denied |
| `E2B_PTY_COMMAND` | `norm-demo` | e2b: typed into the PTY's login shell as `exec <command>`; empty if the template starts it by itself |
| `E2B_PTY_CWD` | `/home/user` | e2b: the PTY's working directory |
| `E2B_DOMAIN` | unset | e2b: a self-hosted E2B's API domain |

## Protocol (`GET /api/tty`, WebSocket)

Requires a valid `sid` cookie (401) and an `Origin` that is the service's own
or in `ALLOWED_ORIGINS` (403).

- **Binary frames** — terminal bytes, both directions. The page sends
  xterm.js's `onData` UTF-8-encoded and `onBinary` as latin-1 bytes.
- **Text frames** — JSON control:
  - client → server: `{"type":"hello","cols":C,"rows":R}` (the first
    message; cols 2–1000, rows 2–500), then `{"type":"resize","cols","rows"}`.
  - server → client: `{"type":"status","phase":P,"message"?,"code"?}` with P
    one of `creating` (new sandbox), `resuming` (a paused one), `starting`
    (attaching the terminal), `ready`, `exited` (`code` = exit status),
    `replaced`, `error` (`message` is shown to the visitor).
- **Close codes** — `4000` protocol (no/bad hello, oversized control frame),
  `4001` replaced by another connection, `4002` the sandbox was reset,
  `4003` error (after an `error` status), `4004` the program exited. The page
  reconnects with backoff (0.5 s doubling to 15 s) on any other close.
- **One connection per visitor.** A new one replaces the old: the old gets
  `replaced`, then is closed. Closing detaches (the process keeps running);
  after `PAUSE_GRACE_MS` with no connection the provider pauses the sandbox.
- **Resume.** Attaching to a live process makes it redraw: the provider
  resizes the PTY to rows−1 and back (a SIGWINCH even when the size did not
  change). The local provider also replays its last 256 KiB of output.
- Input sent before `ready` is queued (up to 64 KiB). WebSocket messages are
  capped at 64 KiB, control frames at 4 KiB.

Other routes: `GET /healthz`; `GET /api/features` (`{browser, provider}`, for
the landing page); `GET /api/session` (`{exists, state}` with `state` ∈
`none|running|paused`, never the sid; issues the cookie); `POST /api/reset`
(cookie + same origin; deletes the sandbox, closing its live connection with
`4002`).

## The `SandboxProvider` interface (`server/provider.ts`)

```ts
findOrCreate(sid, {cols, rows, admit, onPhase}) → SandboxHandle  // admit() runs only before a NEW sandbox
SandboxHandle.attach({cols, rows, onData, onExit}) → {write(bytes), resize(cols, rows), detach()}
reset(sid) · pause(sid) · status(sid) → "none"|"running"|"paused" · count() · close?()
```

`attach` reattaches to the terminal process if it is alive, else starts it.
`onExit(code)` with `code === undefined` means the stream broke (e.g. the
sandbox auto-paused), not that the program exited — the page offers a retry.

### E2B calls (`e2b` 2.51.0)

- create: `Sandbox.create(template, {apiKey, timeoutMs, metadata: {app:
  "norm-demo", sid}, envs: {OWALLET_PASSWORD: <random>, TERM, COLORTERM,
  NORM_OWALLET_ENV}, lifecycle: {onTimeout: "pause"}, network: {allowOut:
  OVERPAY_HOSTS, denyOut: ["0.0.0.0/0"]}})`. Auto-pause is
  `lifecycle.onTimeout` in this SDK (there is no top-level `autoPause`
  option); `secure` is deprecated and ignored (every sandbox is secured), so
  it is not passed.
- find: `Sandbox.list({query: {metadata: {app, sid}, state: ["running",
  "paused"]}})` → `nextItems()`; resume: `Sandbox.connect(id, {timeoutMs})`.
- terminal: `sandbox.pty.create({cols, rows, cwd, envs, onData, timeoutMs:
  0})` (the user's login shell; `exec norm-demo` is typed into it), pid
  written to `/home/user/.norm-demo/pty.pid`; later `pty.connect(pid,
  {onData, timeoutMs: 0})`; `pty.sendInput`, `pty.resize`; `handle.wait()`
  for the exit code (`CommandExitError.exitCode`), `handle.disconnect()` to
  detach; `sandbox.setTimeout` as a keepalive while typing.
- pause / reset / count: `sandbox.pause()` (or `Sandbox.pause(id)`),
  `Sandbox.kill(id)`, `Sandbox.list({query: {metadata: {app}, state:
  ["running"]}})`.

The provider is unit-tested against a fake SDK; it has not yet run against
live E2B (no key in the session that wrote it) — see the spike notes in the
plan before relying on: domains in `allowOut`, PTY survival across
pause/connect, `pty.connect` after a resume.

## Security notes

- **The visitor gets a real shell** (norm's bash tool, or just the terminal
  after norm exits) in their own sandbox. What bounds it: the egress
  allowlist (`OVERPAY_HOSTS` only, `denyOut 0.0.0.0/0`), the sandbox
  timeout + auto-pause, `MAX_SANDBOXES`, the per-IP creation rate, the
  provider key's daily budget and Overpay's demo-credit caps. Nothing secret
  of the service enters the sandbox: the owallet password is generated per
  sandbox and stays inside it.
- **Session cookie**: `sid` = 16 random bytes + HMAC-SHA256 (`SESSION_SECRET`),
  `HttpOnly; SameSite=Lax; Path=/; Max-Age` 180 days, `Secure` on https (or
  `COOKIE_SECURE=1`). Verification is constant-time; a malformed or forged
  cookie is just a new visitor. The sid is the only key to a sandbox, so keep
  `SESSION_SECRET` secret and stable.
- **Cross-site WebSocket hijacking**: the browser attaches the cookie to a
  WebSocket handshake from any page, so `/api/tty` (and `POST /api/reset`)
  require a matching `Origin`; a missing Origin is refused.
- **Clickjacking**: HTML responses carry `frame-ancestors 'none'` /
  `X-Frame-Options: DENY`. Links in the terminal open with
  `noopener,noreferrer`.
- **Static files** are resolved inside their root only (no `..`, no NUL).
- Behind a proxy set `TRUST_PROXY` to its hop count, or the per-IP limit sees
  only the proxy's address (and the cookie misses `Secure`).
- **Retention**: E2B keeps paused sandboxes until they are killed, so the
  server sweeps this app's paused ones older than `RETENTION_DAYS` (counted
  from creation) every `SWEEP_INTERVAL_MS`. A visitor back after that starts
  fresh.
