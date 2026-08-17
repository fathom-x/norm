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

/** Base URL of the owallet server. `NORM_OWALLET_URL` overrides the default. */
export function owalletUrl() {
  return (process.env.NORM_OWALLET_URL ?? "http://127.0.0.1:8765").replace(/\/+$/, "")
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

/**
 * Bring up `owallet serve` when it isn't running. Only possible when all of:
 * the URL is loopback (spawning locally for a remote URL makes no sense), the
 * binary is on PATH, the wallet DB exists (`owallet serve` refuses to create
 * one), and OWALLET_PASSWORD is set — without it the child would try to
 * prompt on /dev/tty and fight the TUI for the terminal.
 *
 * Returns whether owallet is reachable afterwards.
 */
async function ensureServer(base: string): Promise<boolean> {
  if (await probe(base)) return true
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

  const port = new URL(base).port || "8765"
  const child = spawn(bin, ["serve", "--port", port], {
    detached: true,
    stdio: "ignore",
    env: process.env,
  })
  child.unref()
  debug(`started \`owallet serve --port ${port}\` (pid ${child.pid})`)

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
