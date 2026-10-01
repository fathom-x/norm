export * as NormBudget from "./norm-budget"

import path from "path"
import fs from "fs/promises"
import { Global } from "./global"

// norm's spending limits, shared by the server-side norm plugin (which sends
// each request's allowances to owallet) and the TUI (`/budget`, the
// sidebar). Three layers:
//
// - DAILY: the provider key norm mints carries owallet's persistent daily
//   budget, which bounds everything the key costs across all conversations.
// - CONVERSATION: per conversation (a root session and its subagents),
//   enforced by owallet per request via `x-owallet-spend-limit-usd`.
// - PER MESSAGE: the most any one message may authorize (each OpenRouter
//   turn's hold, each tool purchase), one setting for every conversation,
//   sent as `x-owallet-request-max-usd`. Unused authorization is refunded;
//   this bounds the worst case, and a message that can't fit is refused by
//   owallet before anything is charged.
//
// Stored as `{ [rootSessionID]: usd | null }` in norm's data dir: a number is
// that conversation's budget, `null` means no per-conversation limit (the
// daily key budget still applies), and a missing entry means the default.

export const DEFAULT_DAILY_BUDGET_USD = 10
export const DEFAULT_CONVERSATION_BUDGET_USD = 2
export const DEFAULT_REQUEST_MAX_USD = 1
/** Request header owallet reads to lower a request's spending allowance. */
export const SPEND_LIMIT_HEADER = "x-owallet-spend-limit-usd"
/** Request header owallet reads as the most one order may authorize. */
export const REQUEST_MAX_HEADER = "x-owallet-request-max-usd"

type Store = Record<string, number | null>

export function file() {
  return path.join(Global.Path.data, "norm-budgets.json")
}

async function read(): Promise<Store> {
  const parsed = await fs
    .readFile(file(), "utf8")
    .then((text) => JSON.parse(text))
    .catch(() => ({}))
  return parsed && typeof parsed === "object" ? (parsed as Store) : {}
}

/** The conversation's budget in USD, or `null` for no per-conversation limit. */
export async function get(rootSessionID: string): Promise<number | null> {
  const store = await read()
  if (!(rootSessionID in store)) return DEFAULT_CONVERSATION_BUDGET_USD
  const value = store[rootSessionID]
  return typeof value === "number" && Number.isFinite(value) && value >= 0 ? value : null
}

export async function set(rootSessionID: string, usd: number | null): Promise<void> {
  const store = await read()
  store[rootSessionID] = usd
  await writeJson(file(), store)
}

// Written by the TUI while the server reads per request: write-then-rename so
// a reader never sees a half-written file.
async function writeJson(target: string, value: unknown): Promise<void> {
  const tmp = `${target}.${process.pid}.tmp`
  await fs.mkdir(path.dirname(target), { recursive: true })
  await fs.writeFile(tmp, JSON.stringify(value, null, 2) + "\n")
  await fs.rename(tmp, target)
}

export function requestMaxFile() {
  return path.join(Global.Path.data, "norm-request-max.json")
}

/** The per-message limit in USD, or `null` for none. Default: $1. */
export async function getRequestMax(): Promise<number | null> {
  const stored = await fs
    .readFile(requestMaxFile(), "utf8")
    .then((text) => JSON.parse(text) as { usd?: unknown })
    .catch(() => undefined)
  if (!stored || typeof stored !== "object" || !("usd" in stored)) return DEFAULT_REQUEST_MAX_USD
  const usd = stored.usd
  return typeof usd === "number" && Number.isFinite(usd) && usd >= 0 ? usd : null
}

export async function setRequestMax(usd: number | null): Promise<void> {
  await writeJson(requestMaxFile(), { usd })
}

/**
 * Parse `/budget` input: "5", "$5", "2.50" → USD; "off" / "none" /
 * "unlimited" → `null`; anything else → `undefined` (invalid).
 */
export function parse(input: string): number | null | undefined {
  const text = input.trim().toLowerCase()
  if (text === "off" || text === "none" || text === "unlimited" || text === "no limit") return null
  const match = text.match(/^\$?\s*(\d+(?:\.\d{1,2})?)$/)
  if (!match) return undefined
  const usd = Number(match[1])
  return Number.isFinite(usd) ? usd : undefined
}

export function format(usd: number | null): string {
  return usd === null ? "no limit" : `$${usd.toFixed(2)}`
}

/**
 * How the budget code reads sessions. The server plugin and the TUI hold
 * different SDK clients (v1 / v2), so each passes a small adapter.
 */
export type SessionAccess = {
  parentOf(sessionID: string): Promise<string | undefined>
  childrenOf(sessionID: string): Promise<string[]>
  /** Sum of the session's own assistant-message costs, in USD. */
  costOf(sessionID: string): Promise<number>
}

/** A conversation is keyed by its root session: subagents share its budget. */
export async function rootOf(access: SessionAccess, sessionID: string): Promise<string> {
  let id = sessionID
  for (let depth = 0; depth < 32; depth++) {
    const parent = await access.parentOf(id)
    if (!parent) return id
    id = parent
  }
  return id
}

/** Everything the conversation has spent: the root and all its subagents. */
export async function spentUsd(access: SessionAccess, rootSessionID: string): Promise<number> {
  let total = 0
  const seen = new Set<string>()
  const queue = [rootSessionID]
  while (queue.length) {
    const id = queue.pop()!
    if (seen.has(id)) continue
    seen.add(id)
    total += await access.costOf(id)
    queue.push(...(await access.childrenOf(id)))
  }
  return total
}

export type Status = { root: string; budget: number | null; spent: number; remaining: number | null }

export async function status(access: SessionAccess, sessionID: string): Promise<Status> {
  const root = await rootOf(access, sessionID)
  const [budget, spent] = await Promise.all([get(root), spentUsd(access, root)])
  return { root, budget, spent, remaining: budget === null ? null : Math.max(0, budget - spent) }
}
