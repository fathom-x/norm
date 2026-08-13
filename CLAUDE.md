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

## Syncing with upstreams

Both halves track a live upstream; keep norm's divergence surgical so
merges stay cheap.

- `scripts/sync-from-opencode.sh` — merge newer `anomalyco/opencode`
  `dev` commits into the repo root (ordinary merge; upstream's full
  history is in this repo).
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
