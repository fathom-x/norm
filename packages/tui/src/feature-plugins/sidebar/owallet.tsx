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
const FETCH_TIMEOUT_MS = 15_000

// Deliberately duplicated from packages/opencode/src/norm/norm.ts
// (owalletUrl/owalletEnv) — the tui package doesn't depend on the opencode
// package, and the mapping is three lines. Keep the two in sync by hand,
// like install.rs's copy of DEFAULT_MODEL.
const ENV_PORTS = { prod: 8765, dev: 8766, staging: 8767 } as const
const DEFAULT_ENV: keyof typeof ENV_PORTS = "staging"

function owalletUrl() {
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
  merchant_credits?: Array<{ seller_slug?: string; organization_slug?: string; balance_cents?: number }>
  key_budget?: {
    daily_budget_usd?: number | null
    spent_today_usd?: number
    remaining_today_usd?: number | null
  }
}

async function fetchStatus(base: string): Promise<OwalletStatus | undefined> {
  const key = await readProviderKey()
  if (!key) return undefined
  const response = await fetch(`${base}/v1/status`, {
    headers: { Authorization: `Bearer ${key}` },
    signal: AbortSignal.timeout(FETCH_TIMEOUT_MS),
  }).catch(() => undefined)
  if (!response?.ok) return undefined
  return response.json().catch(() => undefined)
}

function usd(value: number | null | undefined) {
  if (value === null || value === undefined) return undefined
  return `$${value.toFixed(2)}`
}

function View(props: { api: TuiPluginApi }) {
  const theme = () => props.api.theme.current
  const base = owalletUrl()
  const dashboard = `${base}/wallet`
  const [status, setStatus] = createSignal<OwalletStatus | undefined>(undefined)

  const refresh = () => void fetchStatus(base).then((next) => next && setStatus(next))
  refresh()
  const timer = setInterval(refresh, POLL_MS)
  onCleanup(() => clearInterval(timer))

  const credits = () => status()?.merchant_credits?.filter((row) => (row.balance_cents ?? 0) > 0) ?? []
  const budget = () => status()?.key_budget

  return (
    <box>
      <text fg={theme().text}>
        <b>owallet</b>
      </text>
      <Show when={status()?.usdc_balance !== undefined}>
        <text fg={theme().textMuted}>{status()!.usdc_balance} USDC</text>
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
      <Show when={budget()?.daily_budget_usd != null}>
        <text fg={theme().textMuted}>
          budget <span style={{ fg: theme().text }}>{usd(budget()!.spent_today_usd ?? 0)}</span> /{" "}
          {usd(budget()!.daily_budget_usd)} today
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
