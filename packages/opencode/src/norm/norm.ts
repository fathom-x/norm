export * as Norm from "./norm"

import path from "path"
import os from "os"
import fs from "fs/promises"
import { existsSync } from "fs"
import { spawn, execFile } from "child_process"
import { Global } from "@opencode-ai/core/global"
import type { ConfigV1 } from "@opencode-ai/core/v1/config/config"

// norm is opencode preconfigured for the Overpay owallet-marketplace stack:
// it ships with the `overpay` provider (owallet's `/v1` OpenAI-compatible
// endpoint) and the `owallet` MCP server wired in by default, and on startup
// it will bring up `owallet serve` and mint a provider key when it can do so
// non-interactively. Everything here is a *default*: any user or project
// config merges over it, and NORM_DISABLE=1 turns the whole layer off.

export const PROVIDER_ID = "overpay"
export const MCP_NAME = "owallet"

/** `NORM_DISABLE=1` (or "true") switches off config defaults and bootstrap. */
export function disabled() {
  const flag = process.env.NORM_DISABLE
  return flag === "1" || flag === "true"
}

export type OwalletEnv = "prod" | "dev" | "staging"

// Pre-public-release default: point at owallet's staging environment.
// Flip to "prod" (and drop this comment) when Overpay opens to the public.
const DEFAULT_ENV: OwalletEnv = "staging"

// Mirrors owallet-config's built-in per-environment ports.
const ENV_PORTS: Record<OwalletEnv, string> = { prod: "8765", dev: "8766", staging: "8767" }

/**
 * Which owallet environment norm targets. `NORM_OWALLET_ENV` overrides the
 * default; it picks the default port below and the `--<env>` flag passed to
 * an auto-started `owallet serve`. The staging/dev flags come from owallet's
 * `dev-envs` feature — compiled into the bundled release binaries during the
 * pre-public-release phase (with the staging Overpay URL baked in), so a
 * bare launch works with just OWALLET_PASSWORD.
 */
export function owalletEnv(): OwalletEnv {
  const env = process.env.NORM_OWALLET_ENV
  if (env === "prod" || env === "dev" || env === "staging") return env
  return DEFAULT_ENV
}

/** Base URL of the owallet server. `NORM_OWALLET_URL` overrides the default. */
export function owalletUrl() {
  if (process.env.NORM_OWALLET_URL) return process.env.NORM_OWALLET_URL.replace(/\/+$/, "")
  return `http://127.0.0.1:${ENV_PORTS[owalletEnv()]}`
}

/**
 * The norm config layer, merged in at the *lowest* precedence — every other
 * config source (global, project, env) wins over these entries. Shapes match
 * what `owallet install --opencode-global` would write, so the two paths are
 * interchangeable; the `default` model is owallet's catalog-independent
 * sentinel (`GET /v1/models` always offers it first).
 */
export function defaults(): ConfigV1.Info {
  const base = owalletUrl()
  return {
    provider: {
      [PROVIDER_ID]: {
        npm: "@ai-sdk/openai-compatible",
        name: "Overpay",
        options: { baseURL: `${base}/v1` },
        models: { default: { name: "Overpay marketplace (default)" } },
      },
    },
    mcp: {
      [MCP_NAME]: { type: "remote", url: `${base}/mcp`, enabled: true },
    },
  } as ConfigV1.Info
}

// $HOME first (matching the install script and owallet itself), os.homedir()
// as the fallback — the env var also keeps this testable, since bun caches
// os.homedir() at process start.
function homeDir() {
  return process.env.HOME || os.homedir()
}

/** Where owallet keeps its encrypted DB (mirrors `owallet_db::default_db_path`). */
function owalletDbPath() {
  if (process.env.OWALLET_DB_PATH) return process.env.OWALLET_DB_PATH
  return path.join(homeDir(), ".owallet", "owallet.db")
}

function owalletBinaryName() {
  return process.platform === "win32" ? "owallet.exe" : "owallet"
}

/** The owallet the norm installer manages, alongside the norm binary itself. */
export function bundledOwalletPath() {
  return path.join(homeDir(), ".norm", "bin", owalletBinaryName())
}

/** First owallet on PATH that is NOT the bundled one — a pre-existing install. */
export function systemOwalletPath(): string | undefined {
  const bundled = bundledOwalletPath()
  for (const dir of (process.env.PATH ?? "").split(path.delimiter)) {
    if (!dir) continue
    const candidate = path.join(dir, owalletBinaryName())
    if (candidate === bundled) continue
    if (existsSync(candidate)) return candidate
  }
  return undefined
}

export type OwalletChoice = "system" | "bundled"

function choiceFile() {
  return path.join(Global.Path.data, "owallet-binary.json")
}

export async function readOwalletChoice(): Promise<OwalletChoice | undefined> {
  const parsed = await fs
    .readFile(choiceFile(), "utf8")
    .then((text) => JSON.parse(text))
    .catch(() => undefined)
  if (parsed?.choice === "system" || parsed?.choice === "bundled") return parsed.choice
  return undefined
}

export async function recordOwalletChoice(choice: OwalletChoice): Promise<void> {
  await fs.writeFile(choiceFile(), JSON.stringify({ choice }, null, 2) + "\n")
}

/**
 * The owallet binary the bootstrap should use. A recorded first-run choice
 * wins; without one (or when the chosen binary is gone) this falls back to
 * whatever exists, preferring a pre-existing install over the bundled one —
 * least surprising for someone who was already running their own owallet.
 */
export function resolveOwalletBinary(choice?: OwalletChoice): string | undefined {
  const bundled = existsSync(bundledOwalletPath()) ? bundledOwalletPath() : undefined
  const system = systemOwalletPath()
  if (choice === "bundled") return bundled ?? system
  if (choice === "system") return system ?? bundled
  return system ?? bundled
}

/**
 * True when the first launch found a pre-existing owallet install and the
 * user hasn't yet said whether norm should use it or its own bundled copy.
 * The bundled binary need not be on disk — the installer deliberately skips
 * it when another owallet exists, and only fetches it once a "bundled"
 * choice is recorded — so requiring it here would make the prompt (and that
 * installer branch) unreachable. With no pre-existing install there is
 * nothing to ask: the bundled copy, when present, is the only option.
 */
export async function needsOwalletChoice(): Promise<boolean> {
  if (disabled()) return false
  if ((await readOwalletChoice()) !== undefined) return false
  return systemOwalletPath() !== undefined
}

/**
 * First-run prompt: an existing owallet was found next to norm's bundled
 * one — ask which the bootstrap should use, and remember the answer. `ask`
 * is injected by the CLI layer (UI.input) so this module stays UI-free.
 * No-op outside a TTY or when there is nothing to decide. Either way the
 * wallet database (~/.owallet) is shared — this only picks the server
 * binary norm auto-starts.
 */
export async function firstRunOwalletChoice(ask: (prompt: string) => Promise<string>): Promise<void> {
  if (!process.stdin.isTTY || !process.stdout.isTTY) return
  if (!(await needsOwalletChoice())) return
  const system = systemOwalletPath()
  const bundledInstalled = existsSync(bundledOwalletPath())
  // When the bundled copy isn't on disk (the installer leaves an existing
  // owallet alone until told otherwise), default to the existing install —
  // it's the only one that can run right now.
  const fallback: OwalletChoice = bundledInstalled ? "bundled" : "system"
  process.stderr.write(
    [
      "",
      "Found an existing owallet install:",
      `  existing:  ${system}`,
      `  bundled:   ${bundledOwalletPath()}${
        bundledInstalled ? " (version-matched to norm)" : " (not installed yet)"
      }`,
      `  wallet db: ${owalletDbPath()} — used either way`,
      "",
      "Which owallet should norm run?",
      `  1) the existing install${fallback === "system" ? " (default)" : ""}`,
      `  2) norm's bundled owallet${fallback === "bundled" ? " (default)" : ""}`,
      "",
    ].join("\n"),
  )
  const answer = (await ask("Choice [1/2]: ")).trim()
  const choice: OwalletChoice = answer === "1" ? "system" : answer === "2" ? "bundled" : fallback
  await recordOwalletChoice(choice)
  if (choice === "bundled" && !bundledInstalled) {
    process.stderr.write(
      `Recorded — re-run the norm install one-liner to fetch the bundled owallet;\n` +
        `until then norm keeps using ${system}. Change later in ${choiceFile()}\n`,
    )
    return
  }
  process.stderr.write(
    `Using ${choice === "system" ? system : bundledOwalletPath()} — change later in ${choiceFile()}\n`,
  )
}

/** Locate the owallet binary honoring the recorded first-run choice. */
async function owalletBinary(): Promise<string | undefined> {
  return resolveOwalletBinary(await readOwalletChoice())
}

/**
 * `--staging`/`--dev` selector for spawned owallet commands; prod is
 * owallet's flagless default. The staging/dev flags exist in dev-envs
 * builds only — on a public build the child exits immediately with a usage
 * error, which the callers surface as a failed step.
 */
function envFlagArgs(): string[] {
  return owalletEnv() === "prod" ? [] : [`--${owalletEnv()}`]
}

function walletSetupFile() {
  return path.join(Global.Path.data, "owallet-setup.json")
}

/** True when the user answered "no" to the first-run wallet setup offer. */
export async function readWalletSetupDeclined(): Promise<boolean> {
  const parsed = await fs
    .readFile(walletSetupFile(), "utf8")
    .then((text) => JSON.parse(text))
    .catch(() => undefined)
  return parsed?.declined === true
}

export async function recordWalletSetupDeclined(): Promise<void> {
  await fs.writeFile(walletSetupFile(), JSON.stringify({ declined: true }, null, 2) + "\n")
}

/**
 * True when the first-run wallet setup should be offered: the norm layer is
 * on, no wallet database exists yet, an owallet binary is available to
 * create one, and the user hasn't previously declined the offer.
 */
export async function needsWalletSetup(): Promise<boolean> {
  if (disabled()) return false
  if (existsSync(owalletDbPath())) return false
  if (await readWalletSetupDeclined()) return false
  return (await owalletBinary()) !== undefined
}

/** Run an owallet subcommand on the caller's terminal — prompts, seed-phrase output and all. */
function runInteractive(bin: string, args: string[], env: NodeJS.ProcessEnv): Promise<number> {
  return new Promise((resolve) => {
    const child = spawn(bin, args, { stdio: "inherit", env })
    child.on("error", () => resolve(1))
    child.on("exit", (code) => resolve(code ?? 1))
  })
}

/**
 * First-run wallet setup: an owallet binary is present but there is no
 * wallet database, so the bootstrap could neither start `owallet serve` nor
 * mint a provider key — a fresh install would sit at "cannot connect" with
 * no hint. Offer to run `owallet init` + `owallet generate` right here,
 * before the TUI owns the terminal.
 *
 * The database password is collected once (an exported OWALLET_PASSWORD
 * wins) and handed to the child commands, then kept in this process's env
 * so the bootstrap that runs moments later can auto-start the server and
 * mint the overpay key for this very session. `ask`/`askSecret` are
 * injected by the CLI layer (UI.input / UI.inputSecret) so this module
 * stays UI-free. A decline is recorded and not asked again; an aborted or
 * failed setup is re-offered on the next launch.
 */
export async function firstRunWalletSetup(
  ask: (prompt: string) => Promise<string>,
  askSecret: (prompt: string) => Promise<string>,
): Promise<void> {
  if (!process.stdin.isTTY || !process.stdout.isTTY) return
  if (!(await needsWalletSetup())) return
  const bin = (await owalletBinary())!
  process.stderr.write(
    [
      "",
      "norm routes the overpay provider through a local owallet server, but",
      `there is no wallet database yet (${owalletDbPath()}).`,
      "",
      "Setting one up runs `owallet init` (choose the database password that",
      "encrypts everything at rest) and `owallet generate` (mint a seed",
      "phrase — write it down).",
      "",
    ].join("\n"),
  )
  const answer = (await ask("Set up the wallet now? [Y/n]: ")).trim().toLowerCase()
  if (answer === "n" || answer === "no") {
    await recordWalletSetupDeclined()
    process.stderr.write(
      `Skipping — run \`owallet init\` and \`owallet generate\` yourself when ready.\n` +
        `(Delete ${walletSetupFile()} to see this offer again.)\n`,
    )
    return
  }

  let password = process.env.OWALLET_PASSWORD
  if (!password) {
    for (let attempt = 0; ; attempt++) {
      const first = await askSecret("Choose a database password (encrypts the wallet at rest): ")
      if (first) {
        const confirm = await askSecret("Confirm database password: ")
        if (confirm === first) {
          password = first
          break
        }
      }
      if (attempt >= 2) {
        process.stderr.write("Giving up — norm will offer wallet setup again on the next launch.\n")
        return
      }
      process.stderr.write(first ? "Passwords did not match, try again.\n" : "Password cannot be empty, try again.\n")
    }
  }

  // OWALLET_PASSWORD makes `init` non-interactive and unlocks the DB for
  // `generate`; generate still prompts for the separate per-wallet (web
  // admin) password and prints the seed phrase on the inherited terminal.
  const env = { ...process.env, OWALLET_PASSWORD: password }
  for (const step of ["init", "generate"]) {
    const args = [...envFlagArgs(), step]
    process.stderr.write(`\nRunning \`owallet ${args.join(" ")}\`...\n`)
    const code = await runInteractive(bin, args, env)
    if (code !== 0) {
      process.stderr.write(
        `\`owallet ${step}\` exited with code ${code} — norm will offer setup again on the next launch.\n`,
      )
      return
    }
  }

  // Keep the password in this process's env: the bootstrap that runs next
  // picks it up to start `owallet serve` and mint the provider key. It is
  // not persisted anywhere — export it in the shell to keep auto-start
  // working across launches.
  process.env.OWALLET_PASSWORD = password
  process.stderr.write(
    "\nWallet ready. norm will now start `owallet serve` and mint an overpay\n" +
      "provider key for this session. To keep that automatic across launches,\n" +
      "export OWALLET_PASSWORD in your shell profile (or run `owallet serve`\n" +
      "yourself before starting norm).\n",
  )
}

/**
 * System-prompt addendum appended (by `SystemPrompt.provider`) whenever the
 * active model belongs to the overpay provider. The inherited opencode
 * prompts tell the model to answer capability questions from the opencode
 * docs — wrong for marketplace capabilities, which live in the tools the
 * wallet attaches server-side.
 */
export function systemPrompt(): string {
  return [
    "# norm (Overpay marketplace)",
    "",
    "You are running inside norm, a fork of opencode preconfigured for the",
    "Overpay marketplace. Requests to the `overpay` provider are served by the",
    "user's local owallet server (an OpenAI-compatible endpoint): it routes",
    "chat to a marketplace inference seller and executes tool calls",
    "server-side as real, individually paid marketplace orders (code",
    "execution, web fetch, image generation, wallet and order lookups, and",
    "whatever else is currently listed).",
    "",
    "- The authoritative list of marketplace capabilities is the set of tools",
    "  attached to your request by the wallet — NOT the opencode docs.",
    "  https://opencode.ai documents only the client (TUI, config, keybinds).",
    "  When asked what you can do on this provider, answer from your attached",
    "  tools; do not fetch opencode docs for that.",
    "- Every chat turn and every server-side tool execution spends real money",
    "  from the user's wallet (bounded by per-key budgets and spend caps), so",
    "  avoid redundant tool calls — but a fresh read of volatile state is not",
    "  redundant: wallet balances, budgets, and order statuses change with",
    "  every order, and results in earlier turns are stale (each carries an",
    "  as_of timestamp). When the user asks for current values, call the tool",
    "  again; never answer from a previous tool result.",
    "- The `owallet` MCP server is also attached client-side for wallet",
    "  operations (balances, orders, marketplace browsing).",
  ].join("\n")
}

/** The `overpay` API key from opencode's auth store, if one is stored. */
export async function readProviderKey(): Promise<string | undefined> {
  const store: Record<string, any> = await fs
    .readFile(path.join(Global.Path.data, "auth.json"), "utf8")
    .then((text) => JSON.parse(text))
    .catch(() => ({}))
  const entry = store[PROVIDER_ID]
  if (entry?.type === "api" && typeof entry.key === "string") return entry.key
  return undefined
}

let modelsPromise: Promise<string[] | undefined> | undefined

/**
 * The marketplace's live model list from `GET /v1/models` (needs the server
 * up and a provider key — both normally arranged by `bootstrap`). Memoized
 * per process on success; a failure resolves undefined and is retried on the
 * next call. The norm plugin's `config` hook merges these into the overpay
 * provider's model list so the picker offers more than `default`.
 */
export function marketplaceModels(): Promise<string[] | undefined> {
  modelsPromise ??= (async () => {
    try {
      const key = await readProviderKey()
      if (!key) {
        debug("model discovery skipped — no overpay provider key yet")
        return undefined
      }
      const res = await fetch(`${owalletUrl()}/v1/models`, {
        headers: { authorization: `Bearer ${key}` },
        signal: AbortSignal.timeout(3000),
      })
      if (!res.ok) {
        debug(`model discovery: /v1/models responded ${res.status}`)
        return undefined
      }
      const body: any = await res.json()
      const ids = (Array.isArray(body?.data) ? body.data : [])
        .map((entry: any) => entry?.id)
        .filter((id: any): id is string => typeof id === "string" && id.length > 0)
      return ids.length ? ids : undefined
    } catch (error) {
      debug("model discovery failed:", error)
      return undefined
    }
  })().then((result) => {
    if (!result) modelsPromise = undefined
    return result
  })
  return modelsPromise
}

/** True if anything answers HTTP at `base` — any status counts, only a network error is "down". */
async function probe(base: string, timeoutMs = 1500): Promise<boolean> {
  try {
    await fetch(`${base}/`, { signal: AbortSignal.timeout(timeoutMs), redirect: "manual" })
    return true
  } catch {
    return false
  }
}

function debug(...args: unknown[]) {
  if (process.env.NORM_DEBUG) console.error("[norm]", ...args)
}

function isLoopback(base: string) {
  try {
    const host = new URL(base).hostname
    return host === "127.0.0.1" || host === "localhost" || host === "::1"
  } catch {
    return false
  }
}

/** "owallet 0.1.2" → [0, 1, 2]; undefined for anything unparsable. */
function parseVersion(s: string | undefined): number[] | undefined {
  const m = s?.match(/(\d+)\.(\d+)\.(\d+)/)
  return m ? [Number(m[1]), Number(m[2]), Number(m[3])] : undefined
}

function versionLess(a: number[], b: number[]): boolean {
  for (let i = 0; i < 3; i++) {
    if (a[i] !== b[i]) return a[i] < b[i]
  }
  return false
}

/** The local binary's version via `owallet --version`. */
function binaryVersion(bin: string): Promise<string | undefined> {
  return new Promise((resolve) => {
    execFile(bin, ["--version"], { timeout: 10_000 }, (error, stdout) => {
      resolve(error ? undefined : stdout.trim())
    })
  })
}

/** The running serve's version via GET /health; undefined for serves predating the endpoint. */
async function serveVersion(base: string, timeoutMs = 1500): Promise<string | undefined> {
  try {
    const res = await fetch(`${base}/health`, { signal: AbortSignal.timeout(timeoutMs) })
    if (!res.ok) return undefined
    const body: any = await res.json().catch(() => undefined)
    return typeof body?.version === "string" ? body.version : undefined
  } catch {
    return undefined
  }
}

/**
 * owallet ≥ 0.1.5 accepts `owk_` provider keys as `/mcp` bearers (scopes +
 * daily budget carried onto MCP purchases). Older serves 401 them, which
 * would sever the MCP connection entirely — so the header is only injected
 * once the running serve proves new enough. Unreachable or pre-/health
 * serves read as "no": the anonymous connection keeps working either way.
 */
export async function mcpAcceptsProviderKeys(): Promise<boolean> {
  const version = parseVersion(await serveVersion(owalletUrl()))
  return version !== undefined && !versionLess(version, [0, 1, 5])
}

/** PIDs listening on a local TCP port (POSIX only — lsof). */
function portListeners(port: string): Promise<number[]> {
  return new Promise((resolve) => {
    execFile("lsof", ["-ti", `tcp:${port}`, "-sTCP:LISTEN"], { timeout: 5_000 }, (error, stdout) => {
      if (error) {
        resolve([])
        return
      }
      resolve(
        stdout
          .split("\n")
          .map((line) => Number(line.trim()))
          .filter((pid) => Number.isInteger(pid) && pid > 0),
      )
    })
  })
}

/**
 * A serve that's already answering may be running an *older* binary than the
 * one on disk: the installer replaces the file, but a running process keeps
 * executing the old code, and the reuse check in `ensureServer` would keep
 * picking it forever (this is how a 0.1.1 serve kept mis-counting budgets
 * after the 0.1.2 upgrade). When the running serve reports a version older
 * than the local binary — or none at all, which means it predates the
 * /health endpoint — bring it down so the normal spawn path below starts
 * the current binary.
 *
 * Only attempted when this bootstrap could actually respawn afterwards
 * (loopback URL, binary + DB + OWALLET_PASSWORD present, POSIX for lsof);
 * otherwise killing would trade a stale serve for none. Returns true when
 * the old serve was brought down and the port is free again.
 */
async function restartIfStale(base: string): Promise<boolean> {
  if (!isLoopback(base)) return false
  if (process.platform === "win32") return false
  const bin = await owalletBinary()
  if (!bin || !existsSync(owalletDbPath()) || !process.env.OWALLET_PASSWORD) return false

  const binVer = parseVersion(await binaryVersion(bin))
  if (!binVer) return false
  const runningVer = parseVersion(await serveVersion(base))
  if (runningVer && !versionLess(runningVer, binVer)) return false

  const port = new URL(base).port || ENV_PORTS[owalletEnv()]
  const pids = await portListeners(port)
  if (!pids.length) {
    debug(`owallet at ${base} looks stale but no listener found on port ${port} — leaving it`)
    return false
  }
  debug(
    `owallet serve at ${base} is ${runningVer ? runningVer.join(".") : "pre-/health (old)"} ` +
      `but the binary is ${binVer.join(".")} — restarting (pids ${pids.join(", ")})`,
  )
  for (const pid of pids) {
    try {
      process.kill(pid, "SIGTERM")
    } catch {}
  }
  const deadline = Date.now() + 4000
  while (Date.now() < deadline) {
    if (!(await probe(base, 400))) return true
    await new Promise((resolve) => setTimeout(resolve, 200))
  }
  debug("old owallet serve did not exit within 4s — leaving it running")
  return false
}

/**
 * Bring up `owallet serve` when it isn't running. Only possible when all of:
 * the URL is loopback (spawning locally for a remote URL makes no sense), the
 * binary is on PATH, the wallet DB exists (`owallet serve` refuses to create
 * one), and OWALLET_PASSWORD is set — without it the child would try to
 * prompt on /dev/tty and fight the TUI for the terminal.
 *
 * A serve that is already answering is reused — unless it turns out to be
 * running an older version than the binary on disk, in which case
 * `restartIfStale` brings it down first and the spawn path below replaces it.
 *
 * Returns whether owallet is reachable afterwards.
 */
async function ensureServer(base: string): Promise<boolean> {
  if (await probe(base)) {
    if (!(await restartIfStale(base))) return true
  }
  if (!isLoopback(base)) {
    debug(`owallet at ${base} is not reachable and not loopback — not spawning`)
    return false
  }
  const bin = await owalletBinary()
  if (!bin) {
    debug("owallet binary not found — skipping auto-start")
    return false
  }
  if (!existsSync(owalletDbPath())) {
    debug("no owallet wallet database yet — run `owallet init` and `owallet generate` first")
    return false
  }
  if (!process.env.OWALLET_PASSWORD) {
    debug("OWALLET_PASSWORD not set — cannot start owallet non-interactively; run `owallet serve` yourself")
    return false
  }

  const port = new URL(base).port || ENV_PORTS[owalletEnv()]
  const args = [...envFlagArgs(), "serve", "--port", port]
  const child = spawn(bin, args, {
    detached: true,
    stdio: "ignore",
    env: process.env,
  })
  child.unref()
  debug(`started \`owallet ${args.join(" ")}\` (pid ${child.pid})`)

  // The child unlocks the DB (PBKDF2) before binding; give it a few seconds.
  const deadline = Date.now() + 6000
  while (Date.now() < deadline) {
    if (await probe(base, 500)) return true
    await new Promise((resolve) => setTimeout(resolve, 250))
  }
  debug("owallet did not become reachable within 6s")
  return false
}

/**
 * Make sure opencode's auth store has an API key for the `overpay` provider,
 * minting one with `owallet provider-key create --json` when possible. The
 * mint opens the encrypted DB directly (it does not need the server), so it
 * only requires the binary, the DB, and OWALLET_PASSWORD.
 *
 * The auth store write mirrors `Auth.set` (same file, shape, and 0600 mode);
 * this runs before the provider registry reads auth.json, so a key minted
 * here is picked up in the same session.
 */
async function ensureProviderKey(): Promise<void> {
  const file = path.join(Global.Path.data, "auth.json")
  const store: Record<string, unknown> = await fs
    .readFile(file, "utf8")
    .then((text) => JSON.parse(text))
    .catch(() => ({}))
  if (store[PROVIDER_ID]) return

  const bin = await owalletBinary()
  if (!bin) return
  if (!existsSync(owalletDbPath())) return
  if (!process.env.OWALLET_PASSWORD) {
    debug("no overpay provider key and OWALLET_PASSWORD unset — paste one via `opencode auth login`")
    return
  }

  const stdout = await new Promise<string | undefined>((resolve) => {
    execFile(
      bin,
      ["provider-key", "create", "--label", "norm", "--json"],
      { env: process.env, timeout: 30_000 },
      (error, stdout, stderr) => {
        if (error) {
          debug("provider-key create failed:", stderr.trim() || error.message)
          resolve(undefined)
          return
        }
        resolve(stdout)
      },
    )
  })
  if (!stdout) return

  const key = JSON.parse(stdout).key
  if (typeof key !== "string" || !key.startsWith("owk_")) {
    debug("provider-key create returned an unexpected payload")
    return
  }
  store[PROVIDER_ID] = { type: "api", key }
  await fs.writeFile(file, JSON.stringify(store, null, 2), { mode: 0o600 })
  debug("minted an overpay provider key and stored it in the auth store")
}

/**
 * The startup bootstrap: called once per instance from the internal norm
 * plugin, before the provider registry loads. Never throws — every step
 * degrades to "leave things as they are" with a NORM_DEBUG note.
 */
export async function bootstrap(): Promise<void> {
  if (disabled()) return
  await ensureServer(owalletUrl()).catch((error) => {
    debug("owallet auto-start failed:", error)
    return false
  })
  await ensureProviderKey().catch((error) => {
    debug("provider key provisioning failed:", error)
  })
}
