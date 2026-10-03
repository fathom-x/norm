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

- **Compact session layout** (TUI): the session column has no side or
  bottom padding; the input is a plain "> " in the agent's color (no
  shaded panel, padding row or half-block edge); "agent · model ·
  provider" moved from its own row in the input panel into the hints row
  below it (replacing the cwd); user messages are one row; and the sidebar shows
  the working directory (with branch) under the title instead of the
  session id. The home route is laid out as an empty session (no logo,
  tips or home footer; the sidebar shows "New session"), and the cursor
  defaults to a steady block in the muted text color
  (`tui/src/config/index.tsx`), hidden while the new-chat placeholder
  shows. The model picker is the large dialog with a taller list
  (`DialogSelect`'s `tall`), keeps prices in search results, and has no
  "Connect provider" (ctrl+a) action (`component/dialog-model.tsx`);
  in a conversation → toggles its prices between the next message and
  the list price, hinted beside "esc" (`DialogSelect`'s `hint`).
  "Build · model" starts in the column typed text does; while working a
  one-character braille spinner sits two columns left of it (upstream: a
  block sweep that pushed it right). → on an empty prompt toggles the
  sidebar (session route and home). The hints row shows "$spent / $core" — the
  conversation's spend over the wallet's core credits — from one
  `/v1/status` poller the owallet plugin runs for the whole app
  (`component/norm-balance.ts`), re-read when a turn ends. ctrl+c on an empty
  prompt exits only on a second press within 2 s ("ctrl+c again to exit"
  in the hints row; `component/norm-exit.ts`, bound in `app.tsx`, and
  dropped from `app_exit`'s defaults in `config/keybind.ts`).
  Edits: `routes/home.tsx`, `tui/src/component/prompt/index.tsx` (`Meta`),
  `routes/session/index.tsx`, `routes/session/sidebar.tsx`,
  `feature-plugins/sidebar/footer.tsx` — expect conflicts there on syncs.

- **apply_patch for every model** (`src/tool/registry.ts`): upstream
  offers the diff-editing `apply_patch` tool only to GPT models (instead
  of edit/write); norm offers it to all of them, with edit/write kept
  beside it except on GPT. Test in `test/tool/registry.test.ts`.

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
  
## Testing norm as an agent

Don't hand manual testing back to the user: norm is fully drivable without
the TUI. The `norm-test` skill (`.claude/skills/norm-test/SKILL.md`) has the
recipes; the short version:

- `scripts/norm-dev` runs this checkout's source in the current directory
  (`bun dev` cds into `packages/opencode`, which puts sessions in the wrong
  project). Always under your own absolute `NORM_HOME`, and never touch
  norm/owallet processes you didn't start (check `ps`/`ss -ltn` first).
- `norm run --format json "<msg>"` is one turn as JSONL; `-s <sessionID>`
  resumes, `--fork` branches. Permission prompts are auto-rejected without
  `--auto`. The question tool is denied unless `--ask`/`--answer` (norm's
  addition to upstream's `run.ts`, marked `// norm:`; test in
  `test/cli/run/run-question.test.ts`): `--answer` scripts answers;
  `--ask` stops at a question with exit code 3, and on a `norm serve` you
  `--attach` to it stays pending for `-s <id> --answer <label>`.
- `norm debug norm` — the norm layer's state as JSON (serve, key
  fingerprint, `/v1/status`, models); `--bootstrap` starts serve/mints first.
- `norm budget [sessionID] [--set <usd|off>] [--request-max <usd|off>]` —
  the TUI's `/budget` and sidebar spend figures (`src/cli/cmd/budget.ts`).
- `scripts/fake-owallet` (`packages/opencode/script/fake-owallet.ts`): a
  fake owallet on the sandbox's port. It has a scripted model and owallet's
  spend rules (`charged_cents`, budget-header refusals, daily budget), plus a
  request log. This is the default for testing norm-side changes: no wallet,
  no Overpay link, and no real money, which staging does spend. End-to-end
  test: `test/norm/fake-owallet.test.ts`.
- TUI-only checks: tmux on a private socket + `capture-pane -p`.
- Only real-owallet/Overpay testing needs a human, who links a wallet to
  Overpay through a browser login. Ask once for a linked agent sandbox and
  reuse its `NORM_HOME`.

- **Safer file writes and planning** (two one-spot edits):
  - `src/tool/write.ts` refuses to overwrite an existing file this session
    never read, or one modified on disk since the session last read or
    wrote it (`src/norm/write-guard.ts`, from the session's own tool
    history; write.txt always claimed this, upstream didn't enforce it).
    edit is unchanged: it must match the file's exact text anyway.
  - The plan agent (`src/agent/agent.ts`) asks before shell commands
    other than a read-only list (`PLAN_READONLY_COMMANDS`), and before
    any redirect — upstream denied only the edit tools, so `sed -i` /
    `echo >` still changed files while planning. User `permission`
    config still wins.

## Rebrand

The fork installs as **`norm`**, side-by-side-safe with a stock
opencode: the binary is `norm` (`packages/opencode/package.json` bin →
`bin/norm`, yargs `scriptName`), and the app identity in
`packages/core/src/global.ts` is `norm`, so all XDG state is norm's
own (`~/.config/norm`, `~/.local/share/norm` incl. `auth.json`, cache,
state). There is no ASCII-art logo: the banner is "Norm <version>"
(`cli/ui.ts` `logo()`, also printed by `norm`/`norm tui` ahead of the
first-run wallet prompts in `cli/cmd/tui.ts`), the TUI home logo is the
word "Norm" (`tui/src/component/logo.tsx`), and the exit summary is just
the session lines (`tui/src/util/presentation.ts`); the terminal window title is
"Norm" / "Norm | <session title>" (`tui/src/app.tsx`).

The primary agent the user picks with tab (Build/Plan) is labelled a
**mode** in the TUI — hints row, "Switch mode" / `/modes` (`/agents`
still works), "Select mode", the keybind descriptions and tips
(`tui/src/app.tsx`, `component/dialog-agent.tsx`,
`component/prompt/index.tsx`, `config/keybind.ts`,
`feature-plugins/home/tips-view.tsx`). Code, config keys (`agent`,
`.opencode/agents/`), command names (`agent.cycle`), subagents and
`norm agent` keep upstream's "agent".

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
- `.github/workflows/norm-release.yml` / `owallet-release.yml` — see
  Installing above; bare `v*` tags are the fork's, `owallet-v*` are
  owallet's.
- Upstream opencode's workflows (hourly `beta`, publish/deploy, issue
  triage, Blacksmith-runner CI…) are deliberately **deleted** — they
  targeted upstream's infra/secrets/runners and queued or failed here.
  An opencode sync that re-adds or modifies them shows modify/delete
  conflicts: resolve by keeping them deleted (cherry-pick anything
  genuinely useful into a `norm-*` workflow instead).
