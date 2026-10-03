import { describe, expect, test } from "bun:test"
import { createMgmtGate, MGMT_GLOBAL, MGMT_HEADER, publishMgmtHeaders } from "../../src/mgmt-gate"

const owallet = async (request: Request) => Response.json({ path: new URL(request.url).pathname })

describe("/_mgmt capability gate", () => {
  const gate = createMgmtGate("secret")
  const route = gate.wrap(owallet)

  test("/_mgmt without the capability is refused", async () => {
    for (const path of ["/_mgmt/status", "/_mgmt/provider-key/create?", "/_mgmt/generate"]) {
      const res = await route(new Request(`http://owallet.internal${path}`, { method: "POST" }))
      expect(res.status).toBe(403)
      expect((await res.json()).error.code).toBe("forbidden")
    }
    const wrong = await route(
      new Request("http://owallet.internal/_mgmt/status", { headers: { [MGMT_HEADER]: "guess" } }),
    )
    expect(wrong.status).toBe(403)
  })

  test("/_mgmt with the capability, and every other route, pass through", async () => {
    const ok = await route(new Request("http://owallet.internal/_mgmt/status", { headers: gate.headers }))
    expect(await ok.json()).toEqual({ path: "/_mgmt/status" })
    for (const path of ["/health", "/v1/models", "/mcp"]) {
      const res = await route(new Request(`http://owallet.internal${path}`))
      expect(res.status).toBe(200)
    }
  })

  test("each boot has its own token, published for norm's host", () => {
    expect(createMgmtGate().headers[MGMT_HEADER]).not.toBe(createMgmtGate().headers[MGMT_HEADER])
    const target: Record<string, unknown> = {}
    publishMgmtHeaders(gate, target)
    expect(target[MGMT_GLOBAL]).toEqual({ [MGMT_HEADER]: "secret" })
  })
})
