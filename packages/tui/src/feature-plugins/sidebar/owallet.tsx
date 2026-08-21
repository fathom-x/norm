import type { TuiPlugin, TuiPluginApi } from "@opencode-ai/plugin/tui"
import type { BuiltinTuiPlugin } from "../builtins"
import { createSignal, For, onCleanup, Show } from "solid-js"
import { Global } from "@opencode-ai/core/global"
import path from "node:path"
import fs from "node:fs/promises"
import open from "open"

const id = "internal:sidebar-owallet"

// norm: how often the widget re-reads `GET /v1/status`. The read is not
// free on the owallet side (EVM RPC + a live Overpay fetch + a Zcash
// sync-on-read), so this stays on the order of a minute.
const POLL_MS = 60_000
// Until the first successful read, failures retry faster: the bootstrap
// may still be starting the serve / minting the provider key when this
// widget mounts, and none of the failure modes below cost a status read.
const RETRY_MS = 10_000
const FETCH_TIMEOUT_MS = 15_000

// Deliberately duplicated from packages/opencode/src/norm/norm.ts
// (owalletUrl/owalletEnv/normHome/sandboxPort) — the tui package doesn't
// depend on the opencode package, and the mapping is small. Keep the two in
// sync by hand, like install.rs's copy of DEFAULT_MODEL. The NORM_HOME
// branch must match exactly: when this copy lagged behind, the widget
// polled the real serve on 8767 with the sandbox's key and rendered
// "provider key rejected" for every sandboxed run.
const ENV_PORTS = { prod: 8765, dev: 8766, staging: 8767 } as const
const DEFAULT_ENV: keyof typeof ENV_PORTS = "staging"
const SANDBOX_PORT_BASE = 8800
const SANDBOX_PORT_SPAN = 1000

function normHome(): string | undefined {
  const value = process.env.NORM_HOME?.trim()
  return value ? path.resolve(value) : undefined
}

/** Same djb2-style hash as norm.ts — the two must land on the same port. */
function sandboxPort(root: string): string {
  let hash = 5381
  for (let i = 0; i < root.length; i++) hash = ((hash * 33) ^ root.charCodeAt(i)) >>> 0
  return String(SANDBOX_PORT_BASE + (hash % SANDBOX_PORT_SPAN))
}

function owalletUrl() {
  // A NORM_HOME sandbox is absolute: its own port, ambient NORM_OWALLET_URL
  // ignored (norm.ts prints the notice).
  const root = normHome()
  if (root) return `http://127.0.0.1:${sandboxPort(root)}`
  if (process.env.NORM_OWALLET_URL) return process.env.NORM_OWALLET_URL.replace(/\/+$/, "")
  const env = process.env.NORM_OWALLET_ENV
  const resolved = env === "prod" || env === "dev" || env === "staging" ? env : DEFAULT_ENV
  return `http://127.0.0.1:${ENV_PORTS[resolved]}`
}

function normDisabled() {
  const value = process.env.NORM_DISABLE
  return value === "1" || value === "true"
}

/** The `overpay` API key from opencode's auth store, if one is stored.
 * Same file and shape `Norm.readProviderKey` reads server-side. */
async function readProviderKey(): Promise<string | undefined> {
  const store: Record<string, any> = await fs
    .readFile(path.join(Global.Path.data, "auth.json"), "utf8")
    .then((text) => JSON.parse(text))
    .catch(() => ({}))
  const entry = store["overpay"]
  if (entry?.type === "api" && typeof entry.key === "string") return entry.key
  return undefined
}

type OwalletStatus = {
  usdc_balance?: string
  eth_balance?: string
  zec_balance?: number | string
  balance_error?: string
  /** The marketplace this wallet points at (env-resolved server-side). */
  overpay_url?: string
  merchant_credits?: Array<{ seller_slug?: string; organization_slug?: string; balance_cents?: number }>
  key_budget?: {
    daily_budget_usd?: number | null
    spent_today_usd?: number
    remaining_today_usd?: number | null
  }
}

/** Why the last status read produced no data — each failure mode renders
 * its own hint line so a blank widget is never a mystery. */
type FetchOutcome =
  | { kind: "ok"; status: OwalletStatus }
  | { kind: "no-key" }
  | { kind: "http"; code: number }
  | { kind: "invalid" }
  | { kind: "timeout" }
  | { kind: "unreachable" }

async function fetchStatus(base: string): Promise<FetchOutcome> {
  const key = await readProviderKey()
  if (!key) return { kind: "no-key" }
  let response: Response
  try {
    response = await fetch(`${base}/v1/status`, {
      headers: { Authorization: `Bearer ${key}` },
      signal: AbortSignal.timeout(FETCH_TIMEOUT_MS),
    })
  } catch (error) {
    return (error as Error)?.name === "TimeoutError" ? { kind: "timeout" } : { kind: "unreachable" }
  }
  if (!response.ok) return { kind: "http", code: response.status }
  const body = await response.json().catch(() => undefined)
  if (!body || typeof body !== "object") return { kind: "invalid" }
  return { kind: "ok", status: body }
}

function stateLine(outcome: FetchOutcome | undefined): string | undefined {
  if (!outcome) return undefined
  switch (outcome.kind) {
    case "ok":
      return undefined
    case "no-key":
      return "no provider key — norm auth login"
    case "http":
      if (outcome.code === 401 || outcome.code === 403) return "provider key rejected — norm auth login"
      if (outcome.code === 404) return "status needs owallet ≥ 0.1.4"
      return `status error (HTTP ${outcome.code})`
    case "invalid":
      return "unexpected status response"
    case "timeout":
      return "status timed out — will retry"
    case "unreachable":
      return "owallet not reachable"
  }
}

function usd(value: number | null | undefined) {
  if (value === null || value === undefined) return undefined
  return `$${value.toFixed(2)}`
}

function View(props: { api: TuiPluginApi }) {
  const theme = () => props.api.theme.current
  const base = owalletUrl()
  const dashboard = `${base}/wallet`
  // The last successful read survives later failures, so stale data stays
  // on screen (with the failure line under it) instead of vanishing.
  const [status, setStatus] = createSignal<OwalletStatus | undefined>(undefined)
  const [outcome, setOutcome] = createSignal<FetchOutcome | undefined>(undefined)

  let timer: ReturnType<typeof setTimeout> | undefined
  let disposed = false
  const refresh = () =>
    void fetchStatus(base).then((next) => {
      if (disposed) return
      setOutcome(next)
      if (next.kind === "ok") setStatus(next.status)
      // A timeout means the serve is mid-read (Zcash sync) — hammering it
      // with retries only queues more of the same expensive read.
      const failedCheaply = next.kind !== "ok" && next.kind !== "timeout"
      timer = setTimeout(refresh, failedCheaply && !status() ? RETRY_MS : POLL_MS)
    })
  refresh()
  onCleanup(() => {
    disposed = true
    if (timer) clearTimeout(timer)
  })

  const credits = () => status()?.merchant_credits?.filter((row) => (row.balance_cents ?? 0) > 0) ?? []
  const budget = () => status()?.key_budget
  const waiting = () => status() === undefined
  const error = () => stateLine(outcome())

  return (
    <box>
      <text fg={theme().text}>
        <b>owallet</b>
      </text>
      {/* Chain-qualified tickers (fathom-x/norm#22): both come from the
          wallet's configured EVM chain — Base for every wallet norm ships.
          If /v1/status ever reports the chain, derive the prefix from it. */}
      <Show when={status()?.usdc_balance !== undefined}>
        <text fg={theme().textMuted}>{status()!.usdc_balance} BASE.USDC</text>
      </Show>
      <Show when={status()?.eth_balance !== undefined}>
        <text fg={theme().textMuted}>{status()!.eth_balance} BASE.ETH</text>
      </Show>
      <Show when={status()?.zec_balance !== undefined}>
        <text fg={theme().textMuted}>{String(status()!.zec_balance)} ZEC</text>
      </Show>
      <Show when={status()?.balance_error}>
        <text fg={theme().warning}>balance unavailable</text>
      </Show>
      <For each={credits()}>
        {(row) => (
          <text fg={theme().textMuted}>
            {row.seller_slug ?? row.organization_slug ?? "credits"}{" "}
            <span style={{ fg: theme().text }}>{usd((row.balance_cents ?? 0) / 100)}</span>
          </text>
        )}
      </For>
      <Show when={status()?.merchant_credits !== undefined && credits().length === 0}>
        <text fg={theme().textMuted}>no merchant credits</text>
      </Show>
      <Show when={budget()}>
        <Show
          when={budget()!.daily_budget_usd != null}
          fallback={
            <text fg={theme().textMuted}>
              budget <span style={{ fg: theme().text }}>{usd(budget()!.spent_today_usd ?? 0)}</span> today · no limit
            </text>
          }
        >
          <text fg={theme().textMuted}>
            budget <span style={{ fg: theme().text }}>{usd(budget()!.spent_today_usd ?? 0)}</span> /{" "}
            {usd(budget()!.daily_budget_usd)} today
          </text>
        </Show>
      </Show>
      {/* Nothing yet: name what the widget is waiting on instead of
          rendering an unexplained blank section (fathom-x/norm#9 follow-up). */}
      <Show when={waiting()}>
        <text fg={theme().textMuted}>balances …</text>
        <text fg={theme().textMuted}>credits …</text>
        <text fg={theme().textMuted}>budget …</text>
      </Show>
      <Show when={error()}>
        <text fg={theme().warning}>{error()}</text>
      </Show>
      <Show when={status()?.overpay_url}>
        <text fg={theme().textMuted} onMouseDown={() => void open(status()!.overpay_url!).catch(() => {})}>
          {status()!.overpay_url}
        </text>
      </Show>
      {/* The dashboard link doubles as the headless port view
          (fathom-x/norm#7): over ssh, this is the address to forward. */}
      <text fg={theme().textMuted} onMouseDown={() => void open(dashboard).catch(() => {})}>
        {dashboard}
      </text>
    </box>
  )
}

const tui: TuiPlugin = async (api) => {
  if (normDisabled()) return
  api.slots.register({
    order: 250,
    slots: {
      sidebar_content() {
        return <View api={api} />
      },
    },
  })
}

const plugin: BuiltinTuiPlugin = {
  id,
  tui,
}

export default plugin
