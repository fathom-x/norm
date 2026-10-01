import { test, expect, beforeEach, afterEach, describe } from "bun:test"
import { Norm } from "@/norm/norm"
import { NormBudget } from "@opencode-ai/core/norm-budget"
import { Global } from "@opencode-ai/core/global"
import path from "path"
import fs from "fs/promises"
import os from "os"

const authFile = () => path.join(Global.Path.data, "auth.json")
const markerFile = () => path.join(Global.Path.data, "overpay-key.json")

describe("NormBudget storage", () => {
  beforeEach(() => fs.rm(NormBudget.file(), { force: true }))
  afterEach(() => fs.rm(NormBudget.file(), { force: true }))

  test("a conversation without an entry gets the default budget", async () => {
    expect(await NormBudget.get("ses_new")).toBe(NormBudget.DEFAULT_CONVERSATION_BUDGET_USD)
  })

  test("set/get round-trips a limit and 'no limit'", async () => {
    await NormBudget.set("ses_a", 5)
    await NormBudget.set("ses_b", null)
    expect(await NormBudget.get("ses_a")).toBe(5)
    expect(await NormBudget.get("ses_b")).toBeNull()
  })

  test("parse accepts amounts and off, rejects the rest", () => {
    expect(NormBudget.parse("5")).toBe(5)
    expect(NormBudget.parse("$2.50")).toBe(2.5)
    expect(NormBudget.parse(" off ")).toBeNull()
    expect(NormBudget.parse("unlimited")).toBeNull()
    expect(NormBudget.parse("five")).toBeUndefined()
    expect(NormBudget.parse("-1")).toBeUndefined()
    expect(NormBudget.parse("1.234")).toBeUndefined()
  })
})

describe("NormBudget.status", () => {
  beforeEach(() => fs.rm(NormBudget.file(), { force: true }))
  afterEach(() => fs.rm(NormBudget.file(), { force: true }))

  // root ─┬─ child ── grandchild
  //       └─ child2
  const tree = {
    parent: { child: "root", child2: "root", grandchild: "child" } as Record<string, string>,
    children: { root: ["child", "child2"], child: ["grandchild"] } as Record<string, string[]>,
    cost: { root: 0.5, child: 0.25, child2: 0.1, grandchild: 0.15 } as Record<string, number>,
  }
  const access: NormBudget.SessionAccess = {
    parentOf: async (id) => tree.parent[id],
    childrenOf: async (id) => tree.children[id] ?? [],
    costOf: async (id) => tree.cost[id] ?? 0,
  }

  test("a subagent session counts against its root conversation's budget", async () => {
    await NormBudget.set("root", 2)
    const status = await NormBudget.status(access, "grandchild")
    expect(status.root).toBe("root")
    expect(status.spent).toBeCloseTo(1.0)
    expect(status.remaining).toBeCloseTo(1.0)
  })

  test("remaining never goes negative, and 'no limit' has none", async () => {
    await NormBudget.set("root", 0.5)
    expect((await NormBudget.status(access, "root")).remaining).toBe(0)
    await NormBudget.set("root", null)
    expect((await NormBudget.status(access, "root")).remaining).toBeNull()
  })
})

describe("norm's provider key", () => {
  let home: string
  let server: ReturnType<typeof Bun.serve>
  let saved: Record<string, string | undefined>
  let savedAuth: string | undefined
  const VARS = ["HOME", "PATH", "NORM_OWALLET_URL", "OWALLET_PASSWORD", "NORM_HOME", "OWALLET_DB_PATH", "OWALLET_HOME", "NORM_DISABLE"]
  // Which keys the fake owallet says can spend.
  let spendKeys: Set<string>

  const argsLog = () => path.join(home, "owallet-args.log")

  beforeEach(async () => {
    home = await fs.mkdtemp(path.join(os.tmpdir(), "norm-key-test-"))
    saved = Object.fromEntries(VARS.map((k) => [k, process.env[k]]))
    for (const k of ["NORM_HOME", "OWALLET_DB_PATH", "OWALLET_HOME", "NORM_DISABLE"]) delete process.env[k]
    process.env.HOME = home
    process.env.PATH = path.join(home, "nowhere")
    process.env.OWALLET_PASSWORD = "pw"
    spendKeys = new Set(["owk_minted_spend_key_0001"])

    // Fake owallet: logs argv, mints a fixed spend key, no version output
    // (so the stale-serve check leaves the fake server alone).
    const bin = path.join(home, ".norm", "bin", "owallet")
    await fs.mkdir(path.dirname(bin), { recursive: true })
    await fs.writeFile(
      bin,
      `#!/bin/sh\necho "$@" >> "${argsLog()}"\ncase "$1" in provider-key) echo '{"key":"owk_minted_spend_key_0001"}' ;; esac\n`,
      { mode: 0o755 },
    )
    await fs.mkdir(path.join(home, ".owallet"), { recursive: true })
    await fs.writeFile(path.join(home, ".owallet", "owallet.db"), "")

    server = Bun.serve({
      port: 0,
      fetch(req) {
        const url = new URL(req.url)
        if (url.pathname !== "/v1/status") return new Response("ok")
        const key = (req.headers.get("authorization") ?? "").replace(/^Bearer /, "")
        return Response.json({ key_can_spend: spendKeys.has(key) })
      },
    })
    process.env.NORM_OWALLET_URL = `http://127.0.0.1:${server.port}`

    savedAuth = await fs.readFile(authFile(), "utf8").catch(() => undefined)
    await fs.rm(authFile(), { force: true })
    await fs.rm(markerFile(), { force: true })
    await fs.rm(path.join(Global.Path.data, "owallet-binary.json"), { force: true })
  })

  afterEach(async () => {
    server.stop(true)
    for (const [k, v] of Object.entries(saved)) {
      if (v === undefined) delete process.env[k]
      else process.env[k] = v
    }
    await fs.rm(markerFile(), { force: true })
    if (savedAuth === undefined) await fs.rm(authFile(), { force: true })
    else await fs.writeFile(authFile(), savedAuth)
    await fs.rm(home, { recursive: true, force: true })
  })

  const writeAuth = (key: string) =>
    fs.writeFile(authFile(), JSON.stringify({ overpay: { type: "api", key } }), { mode: 0o600 })
  const storedKey = async () => JSON.parse(await fs.readFile(authFile(), "utf8")).overpay.key
  const mintCalls = async () =>
    (await fs.readFile(argsLog(), "utf8").catch(() => "")).split("\n").filter((l) => l.startsWith("provider-key"))

  test("a fresh install mints a spend-scoped key with the daily budget", async () => {
    await Norm.bootstrap()
    expect(await storedKey()).toBe("owk_minted_spend_key_0001")
    const [call] = await mintCalls()
    expect(call).toContain("--spend")
    expect(call).toContain(`--budget-usd ${NormBudget.DEFAULT_DAILY_BUDGET_USD}`)
  })

  test("the chat-only key an older norm minted is replaced once", async () => {
    await writeAuth("owk_old_chat_only_key_999")
    await Norm.bootstrap()
    expect(await storedKey()).toBe("owk_minted_spend_key_0001")
    expect(await mintCalls()).toHaveLength(1)
    // Now spend-capable: a second launch leaves it alone.
    await Norm.bootstrap()
    expect(await mintCalls()).toHaveLength(1)
  })

  test("a key the user supplied is never replaced, even if chat-only", async () => {
    await writeAuth("owk_user_pasted_key_5555")
    // norm previously minted a different key.
    await fs.writeFile(markerFile(), JSON.stringify({ fingerprint: "owk_minted_x" }))
    await Norm.bootstrap()
    expect(await storedKey()).toBe("owk_user_pasted_key_5555")
    expect(await mintCalls()).toHaveLength(0)
  })

  test("an existing spend-capable key is kept", async () => {
    spendKeys.add("owk_already_spend_capable")
    await writeAuth("owk_already_spend_capable")
    await Norm.bootstrap()
    expect(await storedKey()).toBe("owk_already_spend_capable")
    expect(await mintCalls()).toHaveLength(0)
  })
})

describe("server-side session access", () => {
  test("sums only assistant costs and follows parents/children via the v1 client", async () => {
    const client = {
      session: {
        get: async ({ path: { id } }: any) => ({ data: { id, parentID: id === "kid" ? "root" : undefined } }),
        children: async ({ path: { id } }: any) => ({ data: id === "root" ? [{ id: "kid" }] : [] }),
        messages: async ({ path: { id } }: any) => ({
          data: [
            { info: { role: "user", cost: 99 } },
            { info: { role: "assistant", cost: id === "root" ? 0.3 : 0.2 } },
          ],
        }),
      },
    }
    const status = await NormBudget.status(Norm.sessionAccess(client), "kid")
    expect(status.root).toBe("root")
    expect(status.spent).toBeCloseTo(0.5)
  })
})

describe("turn cost from owallet's usage", () => {
  test("wallet-tool spend counts toward the turn's cost", async () => {
    const { overpayChargedCents } = await import("@/session/llm/ai-sdk")
    expect(overpayChargedCents({ usage: { charged_cents: 3 } })).toBe(3)
    expect(overpayChargedCents({ usage: { charged_cents: 3, wallet_spent_cents: 500 } })).toBe(503)
    // Older owallet without the field, or junk values, fall back to charged_cents.
    expect(overpayChargedCents({ usage: { charged_cents: 3, wallet_spent_cents: -7 } })).toBe(3)
    expect(overpayChargedCents({ usage: {} })).toBeUndefined()
  })
})
