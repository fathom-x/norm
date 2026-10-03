import type { TuiPlugin, TuiPluginApi } from "@opencode-ai/plugin/tui"
import type { BuiltinTuiPlugin } from "../builtins"
import { createMemo, createSignal, For, onCleanup, Show } from "solid-js"
import { Global } from "@opencode-ai/core/global"
import { NormBudget } from "@opencode-ai/core/norm-budget"
import { NormPricing } from "@opencode-ai/core/norm-pricing"
import { NormAgentModels } from "@opencode-ai/core/norm-agent-models"
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

/** The `owallet` CLI selector matching the environment norm talks to. Plain
 * `owallet` targets prod, so a bare `owallet credits load` on a staging
 * install would load credits into a different Overpay environment. */
function owalletEnvFlag() {
  const env = process.env.NORM_OWALLET_ENV
  const resolved = env === "prod" || env === "dev" || env === "staging" ? env : DEFAULT_ENV
  return resolved === "prod" ? "" : `--${resolved} `
}

/** norm's browser build (packages/web-tui): no dashboard, no on-chain wallet. */
function inBrowser() {
  return process.env.NORM_RUNTIME === "browser"
}

function owalletUrl() {
  // The browser build: owallet is a wasm module behind the page's fetch
  // router (norm.ts's NormHost.BROWSER_OWALLET_URL), whatever NORM_HOME says.
  if (inBrowser()) return "http://owallet.internal"
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

/** Reads `GET /v1/models` into NormPricing for the picker and the sidebar,
 * retrying until it works (the bootstrap may still be minting the key),
 * then hourly — the seller re-prices its catalog about daily. */
function loadPrices() {
  const base = owalletUrl()
  const attempt = async () => {
    const key = await readProviderKey()
    const res = key
      ? await fetch(`${base}/v1/models`, {
          headers: { authorization: `Bearer ${key}` },
          signal: AbortSignal.timeout(FETCH_TIMEOUT_MS),
        }).catch(() => undefined)
      : undefined
    const models = res?.ok ? NormPricing.parseModels(await res.json().catch(() => undefined)) : undefined
    if (models) NormPricing.remember(models)
    setTimeout(() => void attempt(), models ? 60 * 60_000 : RETRY_MS).unref?.()
  }
  void attempt()
}

type OwalletStatus = {
  usdc_balance?: string
  eth_balance?: string
  zec_balance?: number | string
  balance_error?: string
  /** False when the wallet isn't linked to an Overpay account (owallet >= 0.1.10). */
  overpay_connected?: boolean
  /** The marketplace this wallet points at (env-resolved server-side). */
  overpay_url?: string
  merchant_credits?: Array<{
    seller_slug?: string
    organization_slug?: string
    balance_cents?: number
    /** Marks the overpay org's core-credit pool — pinned above everything. */
    core?: boolean
  }>
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

/** The session the TUI is showing, if any. */
function currentSessionID(api: TuiPluginApi): string | undefined {
  const current = api.route.current
  return current.name === "session" && "params" in current ? (current.params?.sessionID as string | undefined) : undefined
}

/** TUI-side SessionAccess over the plugin's (v2) SDK client — same totals
 * the server plugin enforces, subagent sessions included. */
function sessionAccess(api: TuiPluginApi): NormBudget.SessionAccess {
  return {
    async parentOf(sessionID) {
      const res: any = await api.client.session.get({ sessionID })
      return res?.data?.parentID || undefined
    },
    async childrenOf(sessionID) {
      const res: any = await api.client.session.children({ sessionID })
      return (res?.data ?? []).map((s: any) => s.id).filter((id: unknown) => typeof id === "string")
    },
    async costOf(sessionID) {
      const res: any = await api.client.session.messages({ sessionID })
      return (res?.data ?? []).reduce(
        (sum: number, m: any) => sum + (m?.info?.role === "assistant" && Number.isFinite(m.info.cost) ? m.info.cost : 0),
        0,
      )
    },
  }
}

/** `/compaction-model` and `/title-model`: which model norm's housekeeping
 * calls run on. Saved in norm's data and applied server-side by the norm
 * plugin's config hook, which re-runs when instances reload. */
async function openAgentModelDialog(api: TuiPluginApi, agent: NormAgentModels.Agent) {
  const current = (await NormAgentModels.get(agent).catch(() => undefined)) ?? ""
  const overpay = api.state.provider.find((provider) => provider.id === "overpay")
  const models = Object.values(overpay?.models ?? {})
    .filter((model) => model.id !== "default")
    .sort((a, b) => (a.name ?? a.id).localeCompare(b.name ?? b.id))
  const automatic = NormPricing.cheapestForTitles(NormPricing.all())
  const defaults =
    agent === "title"
      ? [
          {
            title: "Automatic — the cheapest model",
            value: "",
            description: automatic ? `default · now ${automatic.name ?? automatic.id}` : "default",
          },
          { title: "Same as the conversation", value: NormAgentModels.CONVERSATION },
        ]
      : [{ title: "Same as the conversation", value: "", description: "default" }]
  const DialogSelect = api.ui.DialogSelect
  api.ui.dialog.replace(() => (
    <DialogSelect
      title={agent === "title" ? "Title model" : "Compaction model"}
      current={current}
      options={[
        ...defaults,
        ...models.map((model) => {
          const pricing = NormPricing.get(model.id)?.pricing
          const context = model.limit?.context
          return {
            title: model.name ?? model.id,
            value: `overpay/${model.id}`,
            // Compaction reads the conversation at nearly the chat model's
            // full window, so a smaller window there can't summarize it.
            description: context ? `${Math.round(context / 1000)}k context` : undefined,
            footer: pricing ? NormPricing.perMillion(pricing) : undefined,
          }
        }),
      ]}
      onSelect={(option) => {
        const model = option.value || undefined
        void NormAgentModels.set(agent, model).then(
          async () => {
            // Reload so the norm plugin re-applies the choice — unless this
            // conversation is mid-reply, which a reload would cut off.
            const sessionID = currentSessionID(api)
            const busy = sessionID ? api.state.session.status(sessionID)?.type === "busy" : false
            if (!busy) await api.client.global.dispose().catch(() => {})
            const which = agent === "title" ? "Titling" : "Compacting"
            const chosen =
              model === undefined && agent === "title"
                ? `${which} with the cheapest model`
                : model === undefined || model === NormAgentModels.CONVERSATION
                  ? `${which} with each conversation's own model`
                  : `${which} with ${option.title}`
            api.ui.toast({
              variant: "info",
              message: `${chosen}${busy ? " — takes effect once this reply finishes and norm restarts." : "."}${
                agent === "compaction"
                  ? " A smaller context window than your chat model's can't summarize a long conversation."
                  : ""
              }`,
            })
          },
          () => api.ui.toast({ variant: "error", message: `Couldn't save the ${agent} model.` }),
        )
        api.ui.dialog.clear()
      }}
    />
  ))
}

/** `/budget`: choose which spending limit to change — this conversation's
 * budget or the per-message limit — then set it. */
async function openBudgetDialog(api: TuiPluginApi) {
  const sessionID = currentSessionID(api)
  const requestMax = await NormBudget.getRequestMax().catch(() => NormBudget.DEFAULT_REQUEST_MAX_USD)
  const current = sessionID ? await NormBudget.status(sessionAccess(api), sessionID).catch(() => undefined) : undefined
  // Outside a conversation only the per-message limit applies.
  if (!current) {
    openRequestMaxDialog(api, requestMax)
    return
  }
  const DialogSelect = api.ui.DialogSelect
  api.ui.dialog.replace(() => (
    <DialogSelect
      title="Spending limits"
      skipFilter
      options={[
        {
          title: "This conversation's budget",
          value: "conversation",
          description:
            current.budget === null
              ? `no limit · ${usd(current.spent)} spent`
              : `${usd(current.remaining ?? 0)} left of ${NormBudget.format(current.budget)}`,
        },
        {
          title: "Per-message limit",
          value: "message",
          description: `${NormBudget.format(requestMax)} · the most one message may authorize`,
        },
      ]}
      onSelect={(option) =>
        option.value === "conversation"
          ? openConversationBudgetDialog(api, current)
          : openRequestMaxDialog(api, requestMax)
      }
    />
  ))
}

function openConversationBudgetDialog(api: TuiPluginApi, current: NormBudget.Status) {
  const DialogPrompt = api.ui.DialogPrompt
  api.ui.dialog.replace(() => (
    <DialogPrompt
      title="Conversation budget"
      description={() => (
        <text>
          Spent {usd(current.spent)} of {NormBudget.format(current.budget)} in this conversation. Enter a new limit
          in USD, or "off" for none. The ${NormBudget.DEFAULT_DAILY_BUDGET_USD}/day cap on Norm's key still applies.
        </text>
      )}
      placeholder="e.g. 5"
      value={current.budget === null ? "off" : String(current.budget)}
      onConfirm={(value) => {
        const next = NormBudget.parse(value)
        if (next === undefined) {
          api.ui.toast({ variant: "error", message: `Not a budget: "${value}". Try 5, 2.50, or off.` })
          return
        }
        void NormBudget.set(current.root, next).then(
          () => {
            api.ui.dialog.clear()
            api.ui.toast({
              variant: "success",
              message:
                next === null
                  ? "No limit for this conversation (daily cap still applies)."
                  : `Budget for this conversation: ${NormBudget.format(next)}.`,
            })
          },
          () => api.ui.toast({ variant: "error", message: "Couldn't save the budget." }),
        )
      }}
      onCancel={() => api.ui.dialog.clear()}
    />
  ))
}

/** The per-message limit: the most one message may authorize. */
function openRequestMaxDialog(api: TuiPluginApi, requestMax: number | null) {
  const DialogPrompt = api.ui.DialogPrompt
  api.ui.dialog.replace(() => (
    <DialogPrompt
      title="Per-message limit"
      description={() => (
        <text>
          The most one message may authorize: {NormBudget.format(requestMax)}. You're charged what a message actually
          costs and the rest comes back; a message that would need more is stopped before anything is charged. Enter
          a limit in USD, or "off" for none. Applies to every conversation.
        </text>
      )}
      placeholder={`e.g. ${NormBudget.DEFAULT_REQUEST_MAX_USD}`}
      value={requestMax === null ? "off" : String(requestMax)}
      onConfirm={(value) => {
        const next = NormBudget.parse(value)
        if (next === undefined) {
          api.ui.toast({ variant: "error", message: `Not a limit: "${value}". Try 1, 0.50, or off.` })
          return
        }
        void NormBudget.setRequestMax(next).then(
          () => {
            api.ui.dialog.clear()
            api.ui.toast({
              variant: "success",
              message:
                next === null
                  ? "No per-message limit (conversation and daily budgets still apply)."
                  : `Per-message limit: ${NormBudget.format(next)}.`,
            })
          },
          () => api.ui.toast({ variant: "error", message: "Couldn't save the limit." }),
        )
      }}
      onCancel={() => api.ui.dialog.clear()}
    />
  ))
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

  // Core credits are the marketplace's primary spend balance: pinned to the
  // very top of the widget (above the chain balances) and shown even at $0.
  // Requires owallet >= 0.1.9 + a Rails deploy that tags the row; without
  // the flag the row renders among the ordinary credits as before.
  const coreCredits = () => status()?.merchant_credits?.find((row) => row.core === true)
  const credits = () =>
    status()?.merchant_credits?.filter((row) => row.core !== true && (row.balance_cents ?? 0) > 0) ?? []
  const budget = () => status()?.key_budget
  const waiting = () => status() === undefined
  const error = () => stateLine(outcome())

  const [chatBudget, setChatBudget] = createSignal<NormBudget.Status>()
  const [requestMax, setRequestMax] = createSignal<number | null>(NormBudget.DEFAULT_REQUEST_MAX_USD)
  const [pricesVersion, setPricesVersion] = createSignal(NormPricing.version())
  const refreshChatBudget = () => {
    setPricesVersion(NormPricing.version())
    void NormBudget.getRequestMax().then(
      (next) => !disposed && setRequestMax(next),
      () => {},
    )
    const sessionID = currentSessionID(props.api)
    if (!sessionID) return setChatBudget(undefined)
    void NormBudget.status(sessionAccess(props.api), sessionID).then(
      (next) => !disposed && setChatBudget(next),
      () => {},
    )
  }
  refreshChatBudget()
  const chatBudgetTimer = setInterval(refreshChatBudget, 5_000)
  onCleanup(() => clearInterval(chatBudgetTimer))

  // What the conversation's next model call would cost on the model it last
  // used — a list-price estimate (the charge is usually lower), from the
  // prices loadPrices() read.
  const nextStep = createMemo(() => {
    pricesVersion()
    const sessionID = currentSessionID(props.api)
    if (!sessionID) return undefined
    const turns = props.api.state.session
      .messages(sessionID)
      .flatMap((message) =>
        message.role === "assistant" && message.providerID === "overpay"
          ? [{ modelID: message.modelID, tokens: message.tokens }]
          : [],
      )
    const modelID = turns.at(-1)?.modelID
    const model = modelID ? NormPricing.get(modelID) : undefined
    const step = model?.pricing ? NormPricing.nextStep(model.pricing, model.id, turns) : undefined
    return step && model ? { usd: step.usd, model: model.name ?? model.id } : undefined
  })

  const needsLogin = () => status()?.overpay_connected === false
  // Every model — ":free" ones included — is paid by redeeming Overpay
  // credits, so a linked wallet with none can't answer a single prompt; the
  // first attempt fails with a raw 422. The status poll already knows the
  // balance, so say it up front, with the exact command for this env.
  const spendableCents = () =>
    (status()?.merchant_credits ?? []).reduce((sum, row) => sum + Math.max(0, row.balance_cents ?? 0), 0)
  const needsCredits = () =>
    !needsLogin() && status()?.merchant_credits !== undefined && spendableCents() === 0

  return (
    <box>
      <text fg={theme().text}>
        <b>owallet</b>
      </text>
      {/* An unlinked wallet can't buy anything — norm's whole point. Say
          so first, ahead of every balance line. */}
      <Show when={needsLogin()}>
        <text fg={theme().warning}>log in to Overpay to get started — owallet authorize</text>
      </Show>
      <Show when={needsCredits()}>
        <text fg={theme().warning}>no Overpay credits — prompts will fail until you load some:</text>
        <text fg={theme().warning}>  owallet {owalletEnvFlag()}credits load --amount-cents 500 --wait</text>
        <text fg={theme().textMuted}>  (Lightning; or top up on the Overpay site below)</text>
      </Show>
      <Show when={chatBudget()}>
        <Show
          when={chatBudget()!.budget !== null && chatBudget()!.remaining === 0}
          fallback={
            <text fg={theme().textMuted}>
              this chat <span style={{ fg: theme().text }}>{usd(chatBudget()!.spent)}</span> /{" "}
              {NormBudget.format(chatBudget()!.budget)} · /budget
            </text>
          }
        >
          <text fg={theme().warning}>
            this chat's {NormBudget.format(chatBudget()!.budget)} budget is used — /budget to raise it
          </text>
        </Show>
        <text fg={theme().textMuted}>
          per message{" "}
          <span style={{ fg: theme().text }}>
            {requestMax() === null ? "no limit" : `≤ ${NormBudget.format(requestMax())}`}
          </span>{" "}
          · /budget
        </text>
        <Show when={nextStep()}>
          <text fg={theme().textMuted}>
            next step ≈ <span style={{ fg: theme().text }}>{NormPricing.money(nextStep()!.usd)}</span> on{" "}
            {nextStep()!.model}
          </text>
        </Show>
      </Show>
      <Show when={coreCredits()}>
        <text fg={theme().textMuted}>
          core credits <span style={{ fg: theme().text }}>{usd((coreCredits()!.balance_cents ?? 0) / 100)}</span>
        </text>
      </Show>
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
      {/* The browser build has no on-chain wallet by design
          (owallet-web reports `unavailable_in_browser`): not a warning. */}
      <Show when={status()?.balance_error && !status()!.balance_error!.startsWith("unavailable_in_browser")}>
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
      <Show when={!needsCredits() && status()?.merchant_credits !== undefined && credits().length === 0}>
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
          (fathom-x/norm#7): over ssh, this is the address to forward. The
          browser build has no dashboard to link to. */}
      <Show when={!inBrowser()}>
        <text fg={theme().textMuted} onMouseDown={() => void open(dashboard).catch(() => {})}>
          {dashboard}
        </text>
      </Show>
    </box>
  )
}

const tui: TuiPlugin = async (api) => {
  if (normDisabled()) return
  loadPrices()
  api.keymap.registerLayer({
    commands: [
      {
        name: "norm.budget",
        title: "Set spending limits",
        slashName: "budget",
        category: "Session",
        namespace: "palette",
        run() {
          void openBudgetDialog(api)
        },
      },
      {
        name: "norm.compaction-model",
        title: "Set compaction model",
        slashName: "compaction-model",
        category: "Session",
        namespace: "palette",
        run() {
          void openAgentModelDialog(api, "compaction")
        },
      },
      {
        name: "norm.title-model",
        title: "Set title model",
        slashName: "title-model",
        category: "Session",
        namespace: "palette",
        run() {
          void openAgentModelDialog(api, "title")
        },
      },
    ],
  })
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
