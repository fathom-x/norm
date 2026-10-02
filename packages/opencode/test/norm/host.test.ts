import { test, expect, beforeEach, afterEach, describe } from "bun:test"
import { Norm } from "@/norm/norm"
import { NormHost } from "@/norm/host"
import { NormBudget } from "@opencode-ai/core/norm-budget"
import { Global } from "@opencode-ai/core/global"
import path from "path"
import fs from "fs/promises"

// The browser build (NORM_RUNTIME=browser): owallet is a wasm module behind
// the page's fetch router, reached at http://owallet.internal — no process,
// no port. These tests stand in for that router with a fake global fetch.

const authFile = () => path.join(Global.Path.data, "auth.json")
const markerFile = () => path.join(Global.Path.data, "overpay-key.json")

type Call = { method: string; url: string; body?: any }

describe("norm in the browser", () => {
  let saved: Record<string, string | undefined>
  let realFetch: typeof fetch
  let calls: Call[]
  let status: NormHost.WasmStatus
  let savedAuth: string | undefined

  beforeEach(async () => {
    saved = { NORM_RUNTIME: process.env.NORM_RUNTIME, NORM_HOME: process.env.NORM_HOME, NORM_DISABLE: process.env.NORM_DISABLE }
    process.env.NORM_RUNTIME = "browser"
    delete process.env.NORM_DISABLE
    realFetch = globalThis.fetch
    calls = []
    status = { version: "0.1.10", initialized: true, unlocked: true, wallet: { npub: "npub1test" }, overpay_linked: true }
    globalThis.fetch = (async (input: any, init?: RequestInit) => {
      const url = typeof input === "string" ? input : input.url
      const method = init?.method ?? "GET"
      calls.push({ method, url, body: init?.body ? JSON.parse(String(init.body)) : undefined })
      if (!url.startsWith(NormHost.BROWSER_OWALLET_URL)) return new Response("not routed", { status: 599 })
      const route = url.slice(NormHost.BROWSER_OWALLET_URL.length)
      if (route === "/_mgmt/status") return Response.json(status)
      if (route === "/_mgmt/provider-key/create")
        return Response.json({ key: "owk_wasm_minted_key_0001", id: 1, npub: "npub1test", label: "norm", scopes: "chat spend" })
      if (route === "/v1/status") return Response.json({ key_can_spend: true })
      return new Response("nope", { status: 404 })
    }) as unknown as typeof fetch
    savedAuth = await fs.readFile(authFile(), "utf8").catch(() => undefined)
    await fs.rm(authFile(), { force: true })
    await fs.rm(markerFile(), { force: true })
  })

  afterEach(async () => {
    globalThis.fetch = realFetch
    for (const [k, v] of Object.entries(saved)) {
      if (v === undefined) delete process.env[k]
      else process.env[k] = v
    }
    await fs.rm(markerFile(), { force: true })
    if (savedAuth === undefined) await fs.rm(authFile(), { force: true })
    else await fs.writeFile(authFile(), savedAuth)
  })

  const storedKey = async () =>
    JSON.parse(await fs.readFile(authFile(), "utf8").catch(() => "{}"))?.overpay?.key as string | undefined
  const mints = () => calls.filter((c) => c.url.endsWith("/_mgmt/provider-key/create"))

  test("owallet lives at the router's private origin, whatever NORM_HOME says", () => {
    process.env.NORM_HOME = "/norm"
    expect(Norm.owalletUrl()).toBe(NormHost.BROWSER_OWALLET_URL)
    expect(Norm.host().kind).toBe("wasm")
  })

  test("defaults point the provider and MCP at owallet-web, with MCP OAuth off", () => {
    const config: any = Norm.defaults()
    expect(config.provider.overpay.options.baseURL).toBe(`${NormHost.BROWSER_OWALLET_URL}/v1`)
    expect(config.mcp.owallet.url).toBe(`${NormHost.BROWSER_OWALLET_URL}/mcp`)
    expect(config.mcp.owallet.oauth).toBe(false)
  })

  test("bootstrap mints norm's spend-scoped key through /_mgmt and stores it", async () => {
    await Norm.bootstrap()
    expect(await storedKey()).toBe("owk_wasm_minted_key_0001")
    expect(mints()).toHaveLength(1)
    expect(mints()[0].body).toEqual({ label: "norm", spend: true, budget_usd: NormBudget.DEFAULT_DAILY_BUDGET_USD })
    // Nothing leaves the page for owallet: every call went to the router's origin.
    expect(calls.every((c) => c.url.startsWith(NormHost.BROWSER_OWALLET_URL))).toBe(true)
    // A second launch keeps the key (it can spend).
    await Norm.bootstrap()
    expect(mints()).toHaveLength(1)
  })

  test("a locked wallet mints nothing — the setup screen unlocks it first", async () => {
    status = { ...status, unlocked: false }
    await Norm.bootstrap()
    expect(await storedKey()).toBeUndefined()
    expect(mints()).toHaveLength(0)
    expect(await Norm.host().mintBlocker()).toContain("locked")
  })

  test("no wallet selected and no database are reported as blockers", async () => {
    status = { ...status, wallet: null }
    expect(await Norm.host().mintBlocker()).toContain("no wallet selected")
    status = { ...status, initialized: false }
    expect(await Norm.host().mintBlocker()).toContain("no wallet database")
  })

  test("an unreachable owallet-web is a blocker, not a crash", async () => {
    globalThis.fetch = (async () => {
      throw new TypeError("fetch failed")
    }) as unknown as typeof fetch
    await Norm.bootstrap()
    expect(await Norm.host().mintBlocker()).toContain("not reachable")
    expect(await storedKey()).toBeUndefined()
  })

  test("the native build keeps the process host", () => {
    delete process.env.NORM_RUNTIME
    expect(Norm.host().kind).toBe("process")
    expect(Norm.owalletUrl()).not.toBe(NormHost.BROWSER_OWALLET_URL)
  })
})
