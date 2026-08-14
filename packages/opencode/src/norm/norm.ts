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

/** Where owallet keeps its encrypted DB (mirrors `owallet_db::default_db_path`). */
function owalletDbPath() {
  if (process.env.OWALLET_DB_PATH) return process.env.OWALLET_DB_PATH
  return path.join(os.homedir(), ".owallet", "owallet.db")
}

/** Locate the owallet binary on PATH. */
function owalletBinary(): string | undefined {
  const name = process.platform === "win32" ? "owallet.exe" : "owallet"
  for (const dir of (process.env.PATH ?? "").split(path.delimiter)) {
    if (!dir) continue
    const candidate = path.join(dir, name)
    if (existsSync(candidate)) return candidate
  }
  return undefined
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
  const bin = owalletBinary()
  if (!bin) {
    debug("owallet binary not on PATH — skipping auto-start")
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

  const bin = owalletBinary()
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
