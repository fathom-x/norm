---
name: norm-test
description: Test norm changes yourself instead of asking a human — drive norm headlessly with `norm run --format json`, resume sessions step by step, read/set budgets and owallet state from the CLI, and capture the TUI as plain text when a visual check is unavoidable. Use whenever you changed norm (the opencode fork or its norm layer) and need to see it work, or are about to ask the user to "try it in norm".
---

# Testing norm as an agent

The fullscreen TUI is the wrong tool for an agent: it needs a terminal, and
its redraws flood your context. Almost everything is reachable from plain CLI
commands that print JSON and exit. Ask a human only for what is listed under
"Needs a human" below.

## 0. Isolation first (always)

Other norm/owallet processes are probably running on this machine (the user's
real wallet, other agents' sandboxes). Never kill, restart or connect to them.

```bash
ps -eo pid,args | grep -E 'owallet|norm' | grep -v grep   # what is already running
ss -ltn                                                     # ports in use
```

- Work in your own git worktree, never in someone else's checkout.
- Run every norm command under a `NORM_HOME` sandbox (absolute path) so
  auth, sessions, budgets, the wallet DB **and the owallet serve port** are
  yours alone. The port is derived from the path (8800–9799):
  `norm debug norm | jq .owallet.url` shows it — if it collides with
  something in `ss -ltn`, pick a different `NORM_HOME` path.
- Never use port 8767/8766/8765 (the real wallet's serve), and don't export
  `NORM_OWALLET_URL` / `OWALLET_*` (a sandbox ignores them anyway, with a
  stderr notice).
- tmux: always a private socket (`tmux -S <scratch>/tmux.sock`), never the
  user's default server.

## 1. Run your build

`scripts/norm-dev` runs this checkout's source **in the current directory**
(`bun dev` cds into `packages/opencode`, so sessions land in the wrong project):

```bash
export NORM_HOME=<abs path>/norm-home      # e.g. under your scratchpad
N=<worktree>/scripts/norm-dev
cd <some project dir>
$N debug norm                               # norm-layer health, JSON (read-only)
$N debug norm --bootstrap                   # same, after starting serve + minting a key
```

`norm run` reads stdin when it isn't a terminal, so give it `</dev/null` in
scripts where stdin might be an open pipe.

### The default: a fake owallet (no wallet, no money)

Agents mostly change norm, not owallet — test against
`scripts/fake-owallet` (`packages/opencode/script/fake-owallet.ts`). It
prepares the sandbox (a fake provider key, a placeholder wallet DB so norm
never runs real wallet setup) and serves owallet's endpoints on the sandbox's
port. norm's own layer runs for real: bootstrap, key check, budget headers,
real-spend cost display, sidebar. Staging spends real money, so prefer this.

```bash
export NORM_HOME=<abs path>/norm-home
<worktree>/scripts/fake-owallet </dev/null > fake.json &   # refuses without NORM_HOME; prints its URL
F=$(jq -r .fake_owallet fake.json)
$N run --format json "hello" </dev/null            # -> "echo: hello", cost = charged_cents (default 1¢)

# Script the next agent replies (norm's title/compaction calls never consume them):
curl -s -X POST $F/_fake/replies -d '[{"tool":{"name":"question","args":{…}}},
  {"text":"Bought 50 credits.","charged_cents":3,"wallet_spent_cents":50}]'
# …or inline in the message:
$N run --format json '<<fake {"text":"pricey","charged_cents":20}>>' </dev/null
# Error replies: {"error":{"status":402,"code":"…","message":"…"}}

curl -s $F/_fake/requests | jq '.[] | {housekeeping, headers}'   # what norm sent
curl -s $F/_fake/state; curl -s -X POST $F/_fake/reset
```

It enforces what owallet does, with owallet's 402 errors: conversation budget
at $0 (`budget_exhausted`), per-message limit and remaining budget
(`request_limit_exceeded`), key daily budget (`--daily-budget USD|off`,
default $10). `/v1/status` reports fake balances and today's spend, so the
sidebar renders. The TUI asks for the wallet password whenever a wallet DB
exists. With the fake, set `OWALLET_PASSWORD=fake`: nothing gets unlocked, and
the fake's high version means norm never restarts it.

What it doesn't test: owallet itself, i.e. real orders, marketplace tools,
OpenRouter. For those, use a linked wallet (see "Needs a human").

`debug norm` answers "why doesn't the overpay provider work": `owallet.binary`,
`db_exists`, `password_set`, `serve_reachable`/`serve_version`,
`overpay_authorized`, `provider_key.fingerprint`, `/v1/status` (`status`,
incl. `key_can_spend`), and the marketplace `models`. `NORM_DEBUG=1` adds
bootstrap diagnostics on stderr.

## 2. Drive conversations step by step — `norm run`

One turn per invocation; it exits when the turn is done. Resume with `-s`.

```bash
$N run --format json -m overpay/default "say hi in 3 words" > turn1.jsonl
SID=$(head -1 turn1.jsonl | jq -r .sessionID)
$N run --format json -s "$SID" "now in French" > turn2.jsonl
$N run --format json -s "$SID" --fork "branch: in German"   # leaves $SID untouched
```

Keep context small — filter, don't cat:

```bash
jq -c 'select(.type=="text") | .part.text' turn1.jsonl
jq -c 'select(.type=="tool_use") | {tool: .part.tool, status: .part.state.status}' turn1.jsonl
jq -s '[.[] | select(.type=="step_finish") | .part.cost] | add' turn1.jsonl   # what the turn cost (USD)
jq -c 'select(.type=="error")' turn1.jsonl
```

Events: `step_start`, `text`, `reasoning` (with `--thinking`), `tool_use`,
`step_finish` (tokens + `cost`; for overpay that is owallet's settled charge,
not a list-price estimate), `error`. Every event carries `sessionID`.

Other flags: `--command <name>` runs a slash command (message = its args),
`--agent`, `-f <file>` attaches files, `--variant`, `--title`.

Non-interactive rules (in `src/cli/cmd/run.ts`): permission prompts are
**auto-rejected** (the tool sees a rejection) unless `--auto`, which approves
everything not explicitly denied — only use `--auto` in a throwaway project
dir. Plan-mode tools are denied. The `question` tool is denied too unless you
pass `--ask` or `--answer`:

```bash
# Scripted: answers are consumed in order by questions asked during the run
# (one --answer per question; a JSON array answers a multi-select).
$N run --format json --answer "Blue" "…"
$N run --format json --answer '["Red","Blue"]' "…"

# Step by step: stop at the question, read it, answer it.
$N serve --port <free port> &
A="--attach http://127.0.0.1:<port>"
$N run $A --ask --format json "…" > t.jsonl        # exit code 3 = waiting on a question
SID=$(head -1 t.jsonl | jq -r .sessionID)
jq -c 'select(.type=="question") | .request.questions' t.jsonl
$N run $A -s "$SID" --answer "Red" --format json  # answers it; the turn continues
```

Each question is a `question` event (`request.questions[]` with `options`;
`answers` when one was supplied). The answer reaches the model as the tool
result, exactly as from the TUI. Questions only stay pending on a server you
`--attach` to: without one, `--ask` dismisses the question (the turn ends,
exit 3) and you reply with a normal message (`-s "$SID" "<answer>"`).
`--ask` on a resumed session re-enables questions for it.

Many turns? Start one server and attach, instead of re-booting per turn:

```bash
$N serve --port <free port> &               # pick a port free in `ss -ltn`
$N run --attach http://127.0.0.1:<port> --format json "…"
```

The server also exposes the full HTTP API (`packages/sdk`) — including
`permission.reply`, if you need to exercise approve/deny paths precisely.

## 3. Inspect state after the fact

```bash
$N session list --format json                     # sessions for this directory
$N export "$SID" | jq '.messages | length'        # full transcript JSON
$N budget [SID]                                   # conversation budget/spend (default: latest session)
$N budget "$SID" --set 0.50                       # what /budget does in the TUI
$N budget "$SID" --set off --request-max 0.25     # no conversation cap; $0.25 per message
$N debug config | jq '.provider.overpay.models | keys'
```

`budget` prints `{session, conversation, budget_usd, spent_usd,
remaining_usd, request_max_usd}` — the same numbers as the TUI sidebar's
"this chat $spent / $budget". Subagent sessions roll up into their root.

## 4. Visual checks (last resort)

For components prefer tests with opentui's test renderer
(`packages/tui/test/**`). When you really must look at the live TUI:

```bash
T="tmux -S $SCRATCH/tmux.sock"
$T new-session -d -s norm -x 160 -y 40 "cd <project> && NORM_HOME=$NORM_HOME OWALLET_PASSWORD=fake OPENCODE_DISABLE_AUTOUPDATE=1 $N"
sleep 20; $T capture-pane -pt norm | sed 's/[[:space:]]*$//' | grep -v '^$'
$T send-keys -t norm '/budget' ; sleep 1; $T send-keys -t norm Enter; sleep 2
$T capture-pane -pt norm | sed 's/[[:space:]]*$//' | grep -v '^$'
$T kill-server                                       # your socket only
```

`capture-pane -p` gives one screen of plain text (no ANSI) — a few dozen lines.
Capture after each step instead of streaming the terminal. The TUI takes
~20s to come up from source; poll until the capture has text. Width 160+
shows the sidebar (owallet balances, "this chat $spent / $budget").

## 5. Needs a human

- **Linking a wallet to Overpay** (`owallet authorize`): a browser OAuth flow.
  Only needed to test against the *real* owallet/Overpay (the fake owallet
  above covers norm-side changes). Real overpay turns need a linked wallet
  and spend real money, staging included. Ask the user once for a long-lived, linked
  agent sandbox (they run `NORM_HOME=<path> norm` interactively and complete
  the browser step), then reuse that `NORM_HOME` — don't use the user's own
  sandboxes or their real wallet without being told to.
- Spending real money beyond the sandbox's budget, prod (`NORM_OWALLET_ENV=prod`) purchases.
- Judging look-and-feel the plain-text capture can't show (colors, alignment).

Without a linked wallet, the fake owallet covers completions, spend and
budgets. Everything else (config, `debug norm`, session plumbing, error paths)
needs no owallet at all.
