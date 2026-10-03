// The broker's configuration, from the environment. Every knob is documented
// in README.md ("Environment"); defaults suit a local run with the local
// provider. `loadConfig` throws ConfigError for anything it cannot use, so a
// misconfigured deployment fails at boot rather than on the first visitor.
import { randomBytes } from "node:crypto"
import { tmpdir } from "node:os"
import path from "node:path"
import { fileURLToPath } from "node:url"

export type ProviderName = "local" | "e2b"

export interface LocalConfig {
  /** The program run in each visitor's PTY. */
  command: string
  args: string[]
  /** Per-visitor state lives under `<root>/<sid>/`. */
  root: string
  /** Extra environment for the program (on top of the per-visitor values). */
  env: Record<string, string>
  /** Names of this process's own env vars passed through (PATH, LANG, …). */
  passEnv: string[]
  /** Kill a visitor's PTY this long after it was paused (0 = never). */
  idleKillMs: number
}

export interface E2BConfig {
  apiKey: string | undefined
  template: string
  /** Sandbox lifetime per connect; on timeout it auto-pauses. */
  timeoutMs: number
  /** The only hosts the sandbox may reach (egress allowlist). */
  overpayHosts: string[]
  /** Typed into the sandbox's login shell to start the demo ("" = none). */
  ptyCommand: string
  ptyCwd: string
  /** E2B API domain (self-hosted); unset = E2B cloud. */
  domain: string | undefined
}

export interface Config {
  host: string
  port: number
  provider: ProviderName
  production: boolean
  sessionSecret: string
  /** True when SESSION_SECRET was not set and a random one is in use. */
  ephemeralSecret: boolean
  /** Always mark the cookie Secure (else only on https requests). */
  cookieSecure: boolean
  /** Extra origins allowed to open the terminal socket / reset. */
  allowedOrigins: string[]
  /** Proxy hops whose X-Forwarded-* (client IP, scheme, host) to trust; 0 = none. */
  trustProxy: number
  webDist: string
  browserDist: string | undefined
  pauseGraceMs: number
  /** Paused sandboxes older than this are deleted; 0 keeps them (E2B's own limits apply). */
  retentionMs: number
  sweepIntervalMs: number
  maxSandboxes: number
  createsPerIpPerHour: number
  helloTimeoutMs: number
  normOwalletEnv: string
  local: LocalConfig
  e2b: E2BConfig
}

export class ConfigError extends Error {
  override readonly name = "ConfigError"
}

const PACKAGE_ROOT = path.resolve(fileURLToPath(new URL("..", import.meta.url)))

export const DEFAULT_PASS_ENV = ["PATH", "LANG", "LC_ALL", "LC_CTYPE", "TMPDIR", "USER", "LOGNAME", "SHELL", "HOME"]

type Env = Record<string, string | undefined>

function int(env: Env, name: string, fallback: number, min = 0, max = Number.MAX_SAFE_INTEGER): number {
  const raw = env[name]?.trim()
  if (!raw) return fallback
  if (!/^\d+$/.test(raw)) throw new ConfigError(`${name} must be a whole number, got ${JSON.stringify(raw)}`)
  const value = Number(raw)
  if (value < min || value > max) throw new ConfigError(`${name} must be between ${min} and ${max}, got ${value}`)
  return value
}

function flag(env: Env, name: string): boolean {
  return ["1", "true", "yes"].includes((env[name] ?? "").trim().toLowerCase())
}

/** TRUST_PROXY: "1"/"true" = one proxy hop, a number = that many, unset = none. */
function hops(env: Env, name: string): number {
  const raw = (env[name] ?? "").trim().toLowerCase()
  if (raw === "true" || raw === "yes") return 1
  return int(env, name, 0, 0, 10)
}

function list(env: Env, name: string, fallback: string[] = []): string[] {
  const raw = env[name]
  if (raw === undefined) return fallback
  return raw
    .split(",")
    .map((item) => item.trim())
    .filter(Boolean)
}

function json<T>(env: Env, name: string, fallback: T, check: (value: unknown) => value is T, shape: string): T {
  const raw = env[name]?.trim()
  if (!raw) return fallback
  let value: unknown
  try {
    value = JSON.parse(raw)
  } catch {
    throw new ConfigError(`${name} must be JSON (${shape})`)
  }
  if (!check(value)) throw new ConfigError(`${name} must be ${shape}`)
  return value
}

const isStringArray = (value: unknown): value is string[] =>
  Array.isArray(value) && value.every((item) => typeof item === "string")
const isStringRecord = (value: unknown): value is Record<string, string> =>
  !!value &&
  typeof value === "object" &&
  !Array.isArray(value) &&
  Object.values(value).every((item) => typeof item === "string")

function origin(value: string, name: string): string {
  try {
    const url = new URL(value)
    if (url.origin === "null") throw new Error()
    return url.origin
  } catch {
    throw new ConfigError(`${name}: ${JSON.stringify(value)} is not an origin (https://host[:port])`)
  }
}

export function loadConfig(env: Env = process.env): Config {
  const provider = (env.SANDBOX_PROVIDER?.trim() || "local") as ProviderName
  if (provider !== "local" && provider !== "e2b")
    throw new ConfigError(`SANDBOX_PROVIDER must be "local" or "e2b", got ${JSON.stringify(provider)}`)
  const production = env.NODE_ENV === "production"

  const secret = env.SESSION_SECRET?.trim()
  if (secret !== undefined && secret !== "" && secret.length < 32)
    throw new ConfigError("SESSION_SECRET must be at least 32 characters")
  if (!secret && (production || provider === "e2b"))
    throw new ConfigError("SESSION_SECRET is required in production and with SANDBOX_PROVIDER=e2b")

  const apiKey = env.E2B_API_KEY?.trim() || undefined
  if (provider === "e2b" && !apiKey) throw new ConfigError("E2B_API_KEY is required with SANDBOX_PROVIDER=e2b")

  const overpayHosts = list(env, "OVERPAY_HOSTS", ["overpay-eykm.onrender.com"])
  for (const host of overpayHosts)
    if (!/^[a-z0-9.-]+$|^[0-9.]+\/\d+$/i.test(host))
      throw new ConfigError(`OVERPAY_HOSTS: ${JSON.stringify(host)} is not a hostname, IP or CIDR`)

  return {
    host: env.HOST?.trim() || "127.0.0.1",
    port: int(env, "PORT", 4330, 0, 65535),
    provider,
    production,
    sessionSecret: secret || randomBytes(32).toString("base64url"),
    ephemeralSecret: !secret,
    cookieSecure: flag(env, "COOKIE_SECURE"),
    allowedOrigins: list(env, "ALLOWED_ORIGINS").map((item) => origin(item, "ALLOWED_ORIGINS")),
    trustProxy: hops(env, "TRUST_PROXY"),
    webDist: path.resolve(env.WEB_DIST?.trim() || path.join(PACKAGE_ROOT, "dist/web")),
    browserDist: env.BROWSER_DIST?.trim() ? path.resolve(env.BROWSER_DIST.trim()) : undefined,
    pauseGraceMs: int(env, "PAUSE_GRACE_MS", 60_000),
    maxSandboxes: int(env, "MAX_SANDBOXES", 20, 1),
    retentionMs: int(env, "RETENTION_DAYS", 7, 0) * 86_400_000,
    sweepIntervalMs: int(env, "SWEEP_INTERVAL_MS", 3_600_000, 60_000),
    createsPerIpPerHour: int(env, "CREATES_PER_IP_PER_HOUR", 5, 1),
    helloTimeoutMs: int(env, "HELLO_TIMEOUT_MS", 10_000, 100),
    normOwalletEnv: env.NORM_OWALLET_ENV?.trim() || "staging",
    local: {
      command: env.SANDBOX_COMMAND?.trim() || "norm-demo",
      args: json(env, "SANDBOX_ARGS", [], isStringArray, "an array of strings"),
      root: path.resolve(env.SANDBOX_ROOT?.trim() || path.join(tmpdir(), "norm-web-sandbox")),
      env: json(env, "SANDBOX_ENV", {}, isStringRecord, "an object of strings"),
      passEnv: list(env, "SANDBOX_PASS_ENV", DEFAULT_PASS_ENV),
      idleKillMs: int(env, "SANDBOX_IDLE_KILL_MS", 0),
    },
    e2b: {
      apiKey,
      template: env.E2B_TEMPLATE?.trim() || "norm-demo",
      timeoutMs: int(env, "E2B_TIMEOUT_MS", 15 * 60_000, 60_000, 24 * 3_600_000),
      overpayHosts,
      ptyCommand: env.E2B_PTY_COMMAND ?? "norm-demo",
      ptyCwd: env.E2B_PTY_CWD?.trim() || "/home/user",
      domain: env.E2B_DOMAIN?.trim() || undefined,
    },
  }
}
