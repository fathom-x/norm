// A fake owallet for testing norm without a wallet linked to Overpay (and
// without spending real money). It answers the endpoints norm and the TUI use
// — /health, /v1/models, /v1/status, /v1/chat/completions, a minimal /mcp —
// and emulates owallet's spend rules: each turn reports `usage.charged_cents`
// (+ `wallet_spent_cents`), the `x-owallet-spend-limit-usd` /
// `x-owallet-request-max-usd` headers and the key's daily budget refuse with
// owallet's 402 error shape, and every request is logged for inspection.
//
// It fakes owallet only: norm's own code (bootstrap, headers, cost display,
// budgets, sidebar) runs for real. owallet's behaviour is not under test.
//
// CLI (via scripts/fake-owallet): prepares a NORM_HOME sandbox and serves on
// its owallet port. Scripting and inspection:
//   POST /_fake/replies   [{ text?, tool?: {name, args}, charged_cents?,
//                           wallet_spent_cents?, error?: {status?, code, message} }]
//                         queued, one per chat request
//   GET  /_fake/requests  chat requests seen (headers norm sent, model, last message)
//   GET  /_fake/state     spend so far, queue length
//   POST /_fake/reset     clear queue, log and spend
// Housekeeping requests (norm's title/compaction/summary calls) skip the
// queue and directives. With an empty queue a chat request is answered by an inline directive in the
// last user message — `<<fake {"text":"…","charged_cents":5}>>` — or else
// echoed back ("echo: …"), or, after a tool call, "tool result: …".

import path from "path"
import fs from "fs/promises"
import { existsSync } from "fs"
import { NormBudget } from "@opencode-ai/core/norm-budget"

export type FakeReply = {
  text?: string
  tool?: { name: string; args: unknown }
  charged_cents?: number
  wallet_spent_cents?: number
  error?: { status?: number; code: string; message: string }
}

export type FakeRequest = {
  time: number
  model?: string
  stream: boolean
  headers: Record<string, string>
  last?: { role: string; content: string }
  tools: string[]
  /** norm's title/compaction/summary calls (`x-owallet-tools: none`). */
  housekeeping: boolean
}

export type FakeOwalletOptions = {
  port: number
  hostname?: string
  /** The key /v1/* accepts. Default: FAKE_KEY. */
  key?: string
  /** Default charge per turn when a reply doesn't say. */
  chargeCents?: number
  /** The key's daily budget; null for none. */
  dailyBudgetUsd?: number | null
}

export const FAKE_KEY = "owk_fake_norm_test_key"
export const FAKE_VERSION = "999.0.0" // never "stale", so norm leaves it running

export function startFakeOwallet(options: FakeOwalletOptions) {
  const key = options.key ?? FAKE_KEY
  const chargeCents = options.chargeCents ?? 1
  const dailyBudgetUsd =
    options.dailyBudgetUsd === undefined ? NormBudget.DEFAULT_DAILY_BUDGET_USD : options.dailyBudgetUsd
  const queue: FakeReply[] = []
  const requests: FakeRequest[] = []
  let spentCents = 0

  const authorized = (req: Request) => req.headers.get("authorization") === `Bearer ${key}`
  const status = () => ({
    overpay_connected: true,
    overpay_url: "fake://overpay",
    usdc_balance: "12.34",
    eth_balance: "0",
    merchant_credits: [{ organization_slug: "overpay", balance_cents: 500, core: true }],
    key_can_spend: true,
    key_budget: {
      daily_budget_usd: dailyBudgetUsd,
      spent_today_usd: spentCents / 100,
      remaining_today_usd: dailyBudgetUsd === null ? null : Math.max(0, dailyBudgetUsd - spentCents / 100),
    },
  })

  const server = Bun.serve({
    port: options.port,
    hostname: options.hostname ?? "127.0.0.1",
    async fetch(req) {
      const url = new URL(req.url)
      const route = `${req.method} ${url.pathname.replace(/\/+$/, "") || "/"}`
      switch (route) {
        case "GET /":
        case "GET /health":
          return Response.json({ status: "ok", version: FAKE_VERSION, fake: true })
        case "GET /_fake/requests":
          return Response.json(requests)
        case "GET /_fake/state":
          return Response.json({ spent_cents: spentCents, queued: queue.length, requests: requests.length })
        case "POST /_fake/replies": {
          const body = await req.json().catch(() => undefined)
          const items = Array.isArray(body) ? body : body ? [body] : []
          queue.push(...items)
          return Response.json({ queued: queue.length })
        }
        case "POST /_fake/reset":
          queue.length = 0
          requests.length = 0
          spentCents = 0
          return Response.json({ ok: true })
        case "POST /mcp":
          return mcp(req)
        case "GET /mcp":
        case "DELETE /mcp":
          return new Response(null, { status: 405 })
      }
      if (!url.pathname.startsWith("/v1/")) return new Response("not found", { status: 404 })
      if (!authorized(req)) return error(401, "invalid_api_key", "invalid provider API key", "authentication_error")
      if (route === "GET /v1/status") return Response.json(status())
      if (route === "GET /v1/models") return Response.json({ object: "list", data: MODELS })
      if (route === "POST /v1/chat/completions") return chat(req)
      return new Response("not found", { status: 404 })
    },
  })

  async function chat(req: Request) {
    const body: any = await req.json().catch(() => ({}))
    const messages: any[] = Array.isArray(body.messages) ? body.messages : []
    const last = messages.at(-1)
    const headers: Record<string, string> = {}
    req.headers.forEach((value, name) => {
      if (name.startsWith("x-")) headers[name] = value
    })
    // Housekeeping calls never take scripted replies, so a queued reply always
    // reaches the agent turn it was meant for.
    const housekeeping = headers["x-owallet-tools"] === "none"
    requests.push({
      time: Date.now(),
      model: body.model,
      stream: Boolean(body.stream),
      headers,
      last: last ? { role: last.role, content: textOf(last.content).slice(0, 500) } : undefined,
      tools: (body.tools ?? []).map((t: any) => t?.function?.name).filter(Boolean),
      housekeeping,
    })

    const reply = (housekeeping ? undefined : (queue.shift() ?? directive(last))) ?? fallback(last)
    if (reply.error) return error(reply.error.status ?? 400, reply.error.code, reply.error.message)

    // owallet's checks, before anything is charged.
    const cents = reply.charged_cents ?? chargeCents
    const total = cents + (reply.wallet_spent_cents ?? 0)
    const limit = usd(headers[NormBudget.SPEND_LIMIT_HEADER])
    const requestMax = usd(headers[NormBudget.REQUEST_MAX_HEADER])
    if (limit !== undefined && limit <= 0)
      return error(
        402,
        "budget_exhausted",
        "This conversation's spending budget is used up, so nothing was sent or charged. Raise it (norm: /budget) or start a new conversation.",
      )
    if (dailyBudgetUsd !== null && spentCents + total > dailyBudgetUsd * 100)
      return error(402, "budget_exhausted", "This key's daily budget is used up.")
    const cap = Math.min(limit ?? Infinity, requestMax ?? Infinity)
    if (total > cap * 100 + 1e-9)
      return error(
        402,
        "request_limit_exceeded",
        `This message would cost $${(total / 100).toFixed(2)}, over its $${cap.toFixed(2)} limit. Nothing was charged.`,
      )
    spentCents += total

    const usage = {
      prompt_tokens: 10,
      completion_tokens: 5,
      total_tokens: 15,
      charged_cents: cents,
      ...(reply.wallet_spent_cents ? { wallet_spent_cents: reply.wallet_spent_cents } : {}),
    }
    const base = {
      id: `chatcmpl-fake-${requests.length}`,
      created: Math.floor(Date.now() / 1000),
      model: body.model ?? "default",
    }
    const toolCall = reply.tool && {
      index: 0,
      id: `call_fake_${requests.length}`,
      type: "function",
      function: { name: reply.tool.name, arguments: JSON.stringify(reply.tool.args ?? {}) },
    }
    const finish = toolCall ? "tool_calls" : "stop"

    if (!body.stream) {
      return Response.json({
        ...base,
        object: "chat.completion",
        choices: [
          {
            index: 0,
            message: { role: "assistant", content: reply.text ?? null, ...(toolCall && { tool_calls: [toolCall] }) },
            finish_reason: finish,
          },
        ],
        usage,
      })
    }
    const chunk = (delta: any, finish_reason: string | null, extra = {}) =>
      `data: ${JSON.stringify({ ...base, object: "chat.completion.chunk", choices: [{ index: 0, delta, finish_reason }], ...extra })}\n\n`
    let sse = chunk({ role: "assistant", content: "" }, null)
    if (reply.text) sse += chunk({ content: reply.text }, null)
    if (toolCall) sse += chunk({ tool_calls: [toolCall] }, null)
    sse += chunk({}, finish, { usage })
    sse += "data: [DONE]\n\n"
    return new Response(sse, { headers: { "content-type": "text/event-stream" } })
  }

  return {
    url: `http://${server.hostname}:${server.port}`,
    port: server.port,
    key,
    requests,
    queue,
    get spentCents() {
      return spentCents
    },
    reply(...items: FakeReply[]) {
      queue.push(...items)
    },
    stop() {
      server.stop(true)
    },
  }
}

const MODELS = [
  {
    id: "default",
    object: "model",
    created: 0,
    owned_by: "overpay",
    name: "Fake marketplace default",
    context_length: 128000,
    pricing: { input: 1, output: 2 },
  },
  {
    id: "fake/cheap",
    object: "model",
    created: 0,
    owned_by: "overpay",
    name: "Fake cheap model",
    context_length: 32000,
    pricing: { input: 0.1, output: 0.2 },
  },
]

function usd(value: string | undefined) {
  if (value === undefined) return undefined
  const parsed = Number(value)
  return Number.isFinite(parsed) ? parsed : undefined
}

function textOf(content: unknown): string {
  if (typeof content === "string") return content
  if (Array.isArray(content))
    return content.map((part: any) => (typeof part?.text === "string" ? part.text : "")).join("")
  return content === undefined || content === null ? "" : JSON.stringify(content)
}

function directive(last: any): FakeReply | undefined {
  if (last?.role !== "user") return
  const match = textOf(last.content).match(/<<fake\s+([\s\S]*?)>>/)
  if (!match) return
  // `norm run` re-quotes a message containing spaces, escaping its quotes.
  for (const candidate of [match[1], match[1].replace(/\\"/g, '"')]) {
    try {
      return JSON.parse(candidate)
    } catch {}
  }
  return { text: `fake-owallet: bad directive JSON: ${match[1].slice(0, 200)}` }
}

function fallback(last: any): FakeReply {
  if (last?.role === "tool") return { text: `tool result: ${textOf(last.content).slice(0, 300)}` }
  return { text: `echo: ${textOf(last?.content).slice(0, 300)}` }
}

function error(
  status: number,
  code: string,
  message: string,
  type = status === 402 ? "insufficient_quota" : "invalid_request_error",
) {
  return Response.json({ error: { message, type, param: null, code } }, { status })
}

// Just enough MCP (streamable HTTP, JSON responses) for norm's `owallet`
// server entry to connect: no tools — the real owallet's tools run
// server-side behind /v1 anyway.
async function mcp(req: Request) {
  const body: any = await req.json().catch(() => undefined)
  const one = (msg: any) => {
    if (!msg || msg.id === undefined) return undefined // notification
    if (msg.method === "initialize")
      return {
        jsonrpc: "2.0",
        id: msg.id,
        result: {
          protocolVersion: msg.params?.protocolVersion ?? "2025-03-26",
          capabilities: { tools: {} },
          serverInfo: { name: "fake-owallet", version: FAKE_VERSION },
        },
      }
    if (msg.method === "tools/list") return { jsonrpc: "2.0", id: msg.id, result: { tools: [] } }
    if (msg.method === "ping") return { jsonrpc: "2.0", id: msg.id, result: {} }
    return { jsonrpc: "2.0", id: msg.id, error: { code: -32601, message: `fake-owallet: ${msg.method} not supported` } }
  }
  const out = Array.isArray(body) ? body.map(one).filter(Boolean) : one(body)
  if (!out || (Array.isArray(out) && !out.length)) return new Response(null, { status: 202 })
  return Response.json(out)
}

/**
 * Make a NORM_HOME sandbox use the fake: the fake key in its auth store and a
 * placeholder wallet DB, so norm neither mints a key nor offers wallet setup
 * (which would run the real owallet). Refuses to replace a real key.
 */
export async function prepareSandbox(root: string, key = FAKE_KEY) {
  const auth = path.join(root, "data", "auth.json")
  const store: Record<string, any> = await fs
    .readFile(auth, "utf8")
    .then((text) => JSON.parse(text))
    .catch(() => ({}))
  const existing = store.overpay?.key
  if (typeof existing === "string" && existing !== key)
    throw new Error(`${auth} already holds a different overpay key — use a fresh NORM_HOME for the fake owallet`)
  store.overpay = { type: "api", key }
  await fs.mkdir(path.dirname(auth), { recursive: true })
  await fs.writeFile(auth, JSON.stringify(store, null, 2), { mode: 0o600 })

  const db = path.join(root, "owallet", "owallet.db")
  if (!existsSync(db)) {
    await fs.mkdir(path.dirname(db), { recursive: true })
    await fs.writeFile(db, "")
  }
}

if (import.meta.main) {
  const root = process.env.NORM_HOME?.trim()
  if (!root) {
    console.error("fake-owallet: set NORM_HOME (an absolute sandbox path) — it never runs against your real norm state")
    process.exit(1)
  }
  const { Norm } = await import("@/norm/norm")
  const sandbox = path.resolve(root)
  const port = Number(new URL(Norm.owalletUrl()).port)
  const arg = (name: string) => {
    const i = process.argv.indexOf(`--${name}`)
    return i > 0 ? process.argv[i + 1] : undefined
  }
  await prepareSandbox(sandbox)
  const daily = arg("daily-budget")
  const fake = startFakeOwallet({
    port,
    chargeCents: arg("charge-cents") ? Number(arg("charge-cents")) : undefined,
    dailyBudgetUsd: daily === undefined ? undefined : daily === "off" ? null : Number(daily),
  })
  console.log(JSON.stringify({ fake_owallet: fake.url, norm_home: sandbox, key: fake.key }))
}
