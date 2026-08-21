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
`NORM_OWALLET_URL` (explicit owallet URL, wins over the env default),
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
*real* wallet's running serve and silently undo the isolation. Inside a
sandbox the owallet binary is picked without prompting — the sandbox's
own `bin/owallet` if the installer put one there, else whatever is on
PATH — so pointing `NORM_HOME` at an empty directory gives fresh state
with the already-installed binary. It is the supported way to exercise a
fresh install (or anything else that would otherwise write to
`~/.owallet`) without touching the real wallet database; read at process
start, so export it before launching. `rm -rf` the directory to undo.


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
`@opencode-ai/*` package names. Known follow-ups: the npm-platform
install path in `bin/norm` still references upstream's `opencode-*`
platform packages, and `norm upgrade` targets upstream releases —
point both at norm's release artifacts.

## Installing

One-line install (root `install` script; downloads the platform binary
from this repo's GitHub releases into `~/.norm/bin` and adds it to
PATH):

```bash
curl -fsSL https://raw.githubusercontent.com/fathom-x/norm/main/install | bash
```

While the repo is private, both the script fetch and the release
download need a token with repo read access:

```bash
curl -fsSL -H "Authorization: Bearer $GITHUB_TOKEN" \
  https://raw.githubusercontent.com/fathom-x/norm/main/install \
  | GITHUB_TOKEN=$GITHUB_TOKEN bash
```

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
- `.github/workflows/norm-release.yml` / `owallet-release.yml` — see
  Installing above; bare `v*` tags are the fork's, `owallet-v*` are
  owallet's.
- Upstream opencode's workflows (hourly `beta`, publish/deploy, issue
  triage, Blacksmith-runner CI…) are deliberately **deleted** — they
  targeted upstream's infra/secrets/runners and queued or failed here.
  An opencode sync that re-adds or modifies them shows modify/delete
  conflicts: resolve by keeping them deleted (cherry-pick anything
  genuinely useful into a `norm-*` workflow instead).
