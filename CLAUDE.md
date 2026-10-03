# CLAUDE.md — norm

norm is a fork of [opencode](https://opencode.ai)
(`anomalyco/opencode`, formerly `sst/opencode`) specialized for the
Overpay owallet-marketplace stack: it is to ship preconfigured to spin
up owallet and connect to Overpay's servers by default.

## Repo layout

- **Repo root** — the opencode fork (TypeScript/bun monorepo; see
  `AGENTS.md` for upstream's own conventions, which still apply to this
  code). Upstream's default branch is `dev`; norm's is `main`.
- **`owallet/`** — the Rust wallet workspace (MCP server, dashboard,
  OAuth AS, `/v1` OpenAI-compatible provider). Forked from
  `fathom-x/overpay`'s `owallet-rs/`. Its own `owallet/CLAUDE.md` is
  the operational guide; cd into `owallet/` for all cargo commands.

## The norm layer

The fork's own behavior lives in `packages/opencode/src/norm/norm.ts`
plus a handful of surgical hook-ins, kept deliberately tiny so opencode
syncs stay cheap:

- `src/config/config.ts` seeds `Norm.defaults()` — the `overpay`
  provider (owallet's `/v1` OpenAI-compatible endpoint via
  `@ai-sdk/openai-compatible`) and the `owallet` remote MCP server —
  at the *lowest* config precedence; any user/project config wins.
- `src/plugin/norm.ts` (registered in `internalPlugins()`) runs
  `Norm.bootstrap()` before providers load: auto-starts `owallet
  serve` and mints a provider key into opencode's auth store via
  `owallet provider-key create --json`, when it can do so
  non-interactively (binary on PATH, wallet DB exists,
  `OWALLET_PASSWORD` set). It also registers the manual
  paste-an-`owk_`-key auth method for `opencode auth login`, and its
  `config` hook merges the marketplace's live model list
  (`Norm.marketplaceModels()`, `GET /v1/models` with the stored key)
  into the overpay provider so the picker offers more than `default`.
  Where owallet reports them (each `/v1/models` entry's `pricing`,
  `context_length`, `active`), `Norm.modelConfig` turns them into the
  model's `cost` and `limit` — without a context limit opencode never
  auto-compacts — and retired models are left out.
- **What models cost** (`packages/core/src/norm-pricing.ts`): parsing,
  list-price estimates (per-turn minimum charge, cache reads,
  long-context tiers) and owallet's refusal floor for the per-message
  limit. The TUI's owallet plugin reads `/v1/models` into it; the model
  picker (`tui/src/component/norm-model-price.ts`, two calls in
  `dialog-model.tsx`) shows list prices, or in a conversation "next ≈ $X"
  / "over your $1 limit" per model and a toast on a mid-conversation
  switch; the sidebar shows the next step's estimate.
- **Housekeeping calls** — titles, compaction, summaries:
  - Models: `/compaction-model` and `/title-model`
    (`packages/core/src/norm-agent-models.ts`), stored in norm's data dir
    and applied by the norm plugin's `config` hook as `agent.<name>.model`
    (`Norm.applyAgentModels`). Compaction defaults to the conversation's
    own model; titles to the marketplace's cheapest
    (`NormPricing.cheapestForTitles`). The user's own config wins, a model
    the marketplace no longer lists is skipped, and the TUI reloads
    instances after a change so the hook re-runs. The same hook sets
    norm's title prompt (`Norm.TITLE_PROMPT`).
  - `chat.headers` sends `x-owallet-tools: none` for these agents
    (`Norm.PLAIN_AGENTS`): a plain completion, without owallet's
    server-side tool roster.
  - Titles retry on each of the first `Norm.TITLE_ATTEMPTS` messages while
    the title is the default (`session/prompt.ts`, upstream titles once).
- **owallet errors in history**: owallet streams refusals/failures as reply
  text (`[owallet error] …`), which opencode stores as an ordinary
  assistant reply. `session/message-v2.ts` drops that text from what the
  model sees and puts `Norm.harnessNote` (a `<system-reminder>`) on the
  next user message instead — replayed as the model's own words, models
  disowned it or took it for something the user wrote.
- `src/session/system.ts` appends `Norm.systemPrompt()` to the system
  prompt for overpay-provider models — the inherited opencode prompts
  send capability questions to the opencode docs, but marketplace
  capabilities live in the tools owallet attaches server-side.
- **Real spend in the cost display** (three one-spot edits, all beside
  upstream's equivalent Copilot handling — keep them together when a
  sync moves that code). opencode estimates cost as tokens x a list
  price, but the overpay provider has no price list and the marketplace
  *knows* the settled charge, so owallet (>= 0.1.5) reports it as a
  `usage.charged_cents` extension and norm spends that number instead:
  `src/session/llm.ts` turns on `includeRawChunks` for the provider (the
  AI SDK's standard usage mapping drops the field, so only raw chunks
  carry it), `src/session/llm/ai-sdk.ts` lifts it out of those chunks
  into `providerMetadata.overpay.chargedCents`, and
  `src/session/session.ts`'s `getUsage` prefers it over the token x price
  arithmetic. Without these the sidebar reads `$0.00 spent` for turns
  that spent real money.

Env knobs: `NORM_DISABLE=1` (turn the layer off), `NORM_OWALLET_ENV`
(`prod`/`dev`/`staging` — picks the default port 8765/8766/8767 and the
`--<env>` flag for auto-started serves; **defaults to `staging` until
the public release**, flip `DEFAULT_ENV` in `norm.ts` then),
`NORM_OWALLET_URL` (explicit owallet URL, wins over the env default —
ignored under `NORM_HOME`),
`NORM_DEBUG=1` (bootstrap diagnostics on stderr), `NORM_HOME` (sandbox
root, below). Staging/dev serve
flags come from owallet's `dev-envs` feature — compiled into release
binaries during the pre-release phase (staging Overpay URL baked in),
see `owallet-release.yml`. Tests:
`packages/opencode/test/config/config.test.ts` (`norm defaults`
describe block) and `packages/opencode/test/norm/norm.test.ts`.

`NORM_HOME=/tmp/example` puts **everything norm owns** under one
directory — `data/` (auth.json, the owallet-binary/setup markers, logs),
`config/`, `cache/`, `state/`, `tmp/`, `owallet/` (the wallet DB plus
owallet's own state and `*.owallet` config, exported to child processes
as `OWALLET_HOME`/`OWALLET_DB_PATH`/`OWALLET_CONFIG_DIR`), and `bin/`
(what the installer writes when the same variable is set). The
auto-started serve also gets its own port, derived from the root path
(8800-9799): defaulting to 8767 would make `ensureServer` reuse the
*real* wallet's running serve and silently undo the isolation. For the
same reason the sandbox is **absolute**: ambient `OWALLET_HOME` /
`OWALLET_DB_PATH` / `OWALLET_CONFIG_DIR` pointing outside the root and
any `NORM_OWALLET_URL` are refused with a stderr notice (a leftover
export from earlier experiments once pointed a "sandboxed" norm at the
real wallet); unset `NORM_HOME` to use them. Inside a
sandbox the owallet binary is picked without prompting — the sandbox's
own `bin/owallet` if the installer put one there, else whatever is on
PATH — so pointing `NORM_HOME` at an empty directory gives fresh state
with the already-installed binary. It is the supported way to exercise a
fresh install (or anything else that would otherwise write to
`~/.owallet`) without touching the real wallet database; read at process
start, so export it before launching. `rm -rf` the directory to undo.

## norm in the browser

`packages/web-tui` runs norm in a browser tab: the real TUI on the page
(xterm.js + opentui on its wasm core from `packages/opentui-wasm`) and the
opencode core in a dedicated Web Worker
(`packages/opencode/src/cli/tui/worker.browser.ts`: the `worker.ts` RPC
surface minus `server`/`snapshot`/`checkUpgrade`, answered by
`HttpApiApp.webHandler()` — never `server/server.ts`). Run everything from
`packages/web-tui`: `bun run build` / `bun run preview`, `bun run test`
(unit, bun + happy-dom), `bun run test:tui` (the TUI in headless Chromium),
`bun run test:browser` (core smoke test via `?debug-panel`). Its README has
the full map; the rules for core and TUI code:

- **Browser variants are new files**, never edits to the native ones:
  `#sqlite`/`#pty`/`#fff` have a `browser` condition (after `bun`/`node`, so
  `bun dev --conditions=browser` is unaffected), and whole modules that are
  platform seams get a twin listed in `TWINS` in
  `packages/web-tui/build/browser-build.ts` (`app-node-platform` →
  `app-browser-platform`, `cross-spawn-spawner`, `ripgrep`, `npm`,
  `tool/shell`, …). A twin keeps the exact exports and Service keys.
- Files are ZenFS (`core/src/effect/vfs-filesystem.ts`), persisted to OPFS;
  SQLite is sqlite-wasm in the OPFS sahpool; there are no processes (every
  spawn fails `NotFound`), so new code that shells out must already tolerate
  a missing binary. Raw `node:*` imports resolve to `src/shims` or to stubs
  that throw only when called. On the page the TUI's files are the worker's
  tree (ZenFS `Port`); sync fs calls there read a cache filled at mount.
- `process.env.NORM_RUNTIME === "browser"` marks the build (seeded by
  `packages/web-tui/src/env.ts`); norm talks to owallet at
  `http://owallet.internal`, answered in-process by the worker's fetch router.
  Anything in the worker can reach that origin (the model's `webfetch`
  included), so owallet-web's `/_mgmt` needs the per-boot capability from
  `packages/web-tui/src/mgmt-gate.ts` (norm's host sends it via
  `NormHost.mgmtHeaders()`), and `/mcp` needs a provider key.

## norm in a cloud sandbox (E2B)

The other demo: the **native** CLI in a per-visitor E2B sandbox, its
terminal streamed to an xterm.js page. `packages/web-sandbox` is the broker
(Bun; holds `E2B_API_KEY`; signed `sid` cookie; WebSocket `/api/tty`, binary
frames = terminal bytes) and the pages (landing, `/sandbox/`; `/browser/` is
the browser build, so one service demos both). Providers: `e2b` (sandbox per
visitor from the `norm-demo` template, auto-pauses when idle, resumed on the
next visit, egress limited to `OVERPAY_HOSTS`, swept after `RETENTION_DAYS`)
and `local` (a PTY on this machine — tests, development; not isolation).
`template/` builds the image (native `norm` + `owallet` under
`NORM_HOME=/home/user/.norm`, the `norm-demo` restart-loop wrapper). The
broker sets `OWALLET_PASSWORD` per sandbox, so norm's first run goes straight
to "connect to Overpay": `Norm.connectOverpay` offers a new account with no
login (`owallet register`, NIP-98) and then the demo credits (`owallet
demo-credits`) — the same choice real CLI users get. Run from
`packages/web-sandbox`: `bun run test` (unit), `bun run test:browser`,
`bun run test:norm` (the real norm through the local provider + mock
Overpay). Deploy: `Dockerfile`, `.github/workflows/norm-demo-image.yml`,
`render.yaml` — its README has the details.

## Rebrand

The fork installs as **`norm`**, side-by-side-safe with a stock
opencode: the binary is `norm` (`packages/opencode/package.json` bin →
`bin/norm`, yargs `scriptName`), and the app identity in
`packages/core/src/global.ts` is `norm`, so all XDG state is norm's
own (`~/.config/norm`, `~/.local/share/norm` incl. `auth.json`, cache,
state). The wordmark/TUI logo spell "norm" (`packages/tui/src/logo.ts`,
`util/presentation.ts`, `cli/ui.ts`).

Deliberately *kept* from upstream for compatibility and cheap merges:
`OPENCODE_*` env vars, `opencode.json`/`opencode.jsonc` config file
names, project `.opencode/` dirs, the `$schema` URL, and internal
`@opencode-ai/*` package names. norm ships only through the `install`
script: `norm upgrade` recognises only installer locations and refuses
package-manager methods (probing for `opencode`/`opencode-ai` would find a
*stock opencode* install), and `bin/norm` resolves `norm-<platform>-<arch>`
packages should npm distribution ever be added.

## Installing

One-line install (root `install` script; downloads the platform binary
from this repo's GitHub releases into `~/.norm/bin` and adds it to
PATH):

```bash
curl -fsSL https://raw.githubusercontent.com/fathom-x/norm/main/install | bash
```

The repository and its releases are public — no token needed. `GITHUB_TOKEN` /
`GH_TOKEN` are still honoured by `install` (they raise the GitHub API rate
limit).

Releases are produced by `.github/workflows/norm-release.yml` on bare
`v*` tags: one ubuntu runner cross-compiles every target via
`packages/opencode/script/build.ts` (artifacts `norm-<os>-<arch>[-baseline][-musl]`
containing a `norm` binary) and uploads them to the tag's GitHub
release. Three ways to cut one: `git tag v0.1.1 && git push origin
v0.1.1`; the workflow_dispatch button (version input); or — from a
remote session whose git proxy only allows branch pushes — `git push
origin main:release/v0.1.1`, where the run mints the tag itself (the
`release/*` branch can be deleted afterwards).

## Syncing with upstreams

Both halves track a live upstream; keep norm's divergence surgical so
merges stay cheap.

- `scripts/sync-from-opencode.sh` — merge upstream's latest `vX.Y.Z`
  release tag into the repo root (ordinary merge; upstream's full
  history is in this repo). Releases, not tip of `dev`, on purpose:
  known-good snapshots. `OPENCODE_REF` overrides.
- `scripts/sync-from-overpay.sh` — merge newer `owallet-rs/` commits
  from `fathom-x/overpay` into `owallet/` (deterministic
  `git subtree split` + `-Xsubtree=owallet` merge; see script header).

Run the relevant test suite after every sync.

## CI / releases

- `.github/workflows/norm-ci.yml` — typecheck for the opencode half
  (push to main + PRs).
- `.github/workflows/owallet-ci.yml` — fmt/clippy/test + Docker build
  for `owallet/**`.
- `.github/workflows/norm-web-ci.yml` — norm in the browser (opentui wasm,
  web-tui suites) and in a cloud sandbox (web-sandbox suites);
  `norm-web-pages.yml` / `norm-demo-image.yml` — manual deploys.
- `.github/workflows/norm-release.yml` / `owallet-release.yml` — see
  Installing above; bare `v*` tags are the fork's, `owallet-v*` are
  owallet's.
- Upstream opencode's workflows (hourly `beta`, publish/deploy, issue
  triage, Blacksmith-runner CI…) are deliberately **deleted** — they
  targeted upstream's infra/secrets/runners and queued or failed here.
  An opencode sync that re-adds or modifies them shows modify/delete
  conflicts: resolve by keeping them deleted (cherry-pick anything
  genuinely useful into a `norm-*` workflow instead).
