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
plus two surgical hook-ins, kept deliberately tiny so opencode syncs
stay cheap:

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
  paste-an-`owk_`-key auth method for `opencode auth login`.

Env knobs: `NORM_DISABLE=1` (turn the layer off), `NORM_OWALLET_URL`
(non-default owallet), `NORM_DEBUG=1` (bootstrap diagnostics on
stderr). Tests: `packages/opencode/test/config/config.test.ts`
(`norm defaults` describe block).

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

- `.github/workflows/owallet-ci.yml` — fmt/clippy/test + Docker build
  for `owallet/**`.
- `.github/workflows/owallet-release.yml` — static `owallet` binaries
  on `owallet-v*` tags. Bare `v*` tags are reserved for the fork's own
  releases.
- The remaining `.github/workflows/*.yml` came from upstream opencode
  and target upstream's infra (publish, deploy, Discord, issue
  triage…). They have not been audited for this repo — expect some to
  fail or no-op without upstream's secrets; disabling/pruning them is a
  known follow-up.
