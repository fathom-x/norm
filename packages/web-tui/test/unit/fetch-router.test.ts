import { describe, expect, test } from "bun:test"
import { installFetchRouter, owalletUnavailable, OWALLET_ORIGIN } from "../../src/fetch-router"

function target() {
  const seen: string[] = []
  return {
    seen,
    fetch: (async (input: RequestInfo | URL) => {
      seen.push(input instanceof Request ? input.url : String(input))
      return new Response("network")
    }) as typeof fetch,
  }
}

describe("installFetchRouter", () => {
  test("routes private origins in-process and passes everything else through", async () => {
    const host = target()
    const router = installFetchRouter(
      { "http://owallet.internal": async (request) => Response.json({ path: new URL(request.url).pathname }) },
      host,
    )
    expect(await (await host.fetch("http://owallet.internal/v1/models")).json()).toEqual({ path: "/v1/models" })
    expect(await (await host.fetch("https://example.com/x")).text()).toBe("network")
    expect(host.seen).toEqual(["https://example.com/x"])
    router.uninstall()
    expect(await (await host.fetch("http://owallet.internal/health")).text()).toBe("network")
  })

  test("preserves method, headers and body for Request and init forms", async () => {
    const host = target()
    const received: { method: string; auth: string | null; body: string }[] = []
    installFetchRouter(
      {
        [OWALLET_ORIGIN]: async (request) => {
          received.push({ method: request.method, auth: request.headers.get("authorization"), body: await request.text() })
          return new Response(null, { status: 204 })
        },
      },
      host,
    )
    await host.fetch(`${OWALLET_ORIGIN}/v1/chat/completions`, {
      method: "POST",
      headers: { authorization: "Bearer owk_1" },
      body: "{}",
    })
    await host.fetch(
      new Request(`${OWALLET_ORIGIN}/mcp`, { method: "POST", headers: { authorization: "Bearer owk_2" }, body: "[]" }),
    )
    expect(received).toEqual([
      { method: "POST", auth: "Bearer owk_1", body: "{}" },
      { method: "POST", auth: "Bearer owk_2", body: "[]" },
    ])
  })

  test("streams response bodies through unchanged", async () => {
    const host = target()
    installFetchRouter(
      {
        [OWALLET_ORIGIN]: async () =>
          new Response(
            new ReadableStream({
              start(controller) {
                controller.enqueue(new TextEncoder().encode("data: 1\n\n"))
                controller.enqueue(new TextEncoder().encode("data: [DONE]\n\n"))
                controller.close()
              },
            }),
            { headers: { "content-type": "text/event-stream" } },
          ),
      },
      host,
    )
    const response = await host.fetch(`${OWALLET_ORIGIN}/v1/chat/completions`)
    expect(response.headers.get("content-type")).toBe("text/event-stream")
    expect(await response.text()).toBe("data: 1\n\ndata: [DONE]\n\n")
  })

  test("routes can be added later and the unavailable stub answers 503", async () => {
    const host = target()
    const router = installFetchRouter({}, host)
    expect(await (await host.fetch(`${OWALLET_ORIGIN}/health`)).text()).toBe("network")
    router.route(OWALLET_ORIGIN, owalletUnavailable)
    const response = await host.fetch(`${OWALLET_ORIGIN}/health`)
    expect(response.status).toBe(503)
    expect((await response.json()).error.code).toBe("owallet_unavailable")
    router.unroute(OWALLET_ORIGIN)
    expect(await (await host.fetch(`${OWALLET_ORIGIN}/health`)).text()).toBe("network")
  })
})
