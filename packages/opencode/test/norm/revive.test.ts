import { describe, expect, test } from "bun:test"
import { Norm } from "../../src/norm/norm"
import { OwalletDownError, OwalletRevive } from "../../src/norm/revive"
import { MessageV2 } from "../../src/session/message-v2"
import { SessionRetry } from "../../src/session/retry"

// A stand-in owallet on a real port, so "down" is a real refused connection.
function owallet() {
  const serve = (port: number) =>
    Bun.serve({
      port,
      hostname: "127.0.0.1",
      fetch: async (request) => Response.json({ echoed: await request.text() }),
    })
  const state = { server: serve(0) as ReturnType<typeof serve> | undefined, starts: 0, revived: 0 }
  const port = state.server!.port!
  const base = `http://127.0.0.1:${port}`
  return {
    base,
    state,
    stop: () => {
      state.server?.stop(true)
      state.server = undefined
    },
    deps: (start?: OwalletRevive.Deps["start"]): OwalletRevive.Deps => ({
      base: () => base,
      reachable: Norm.probe,
      fetch: globalThis.fetch,
      revived: () => {
        state.revived++
      },
      start:
        start ??
        (async () => {
          state.starts++
          state.server ??= serve(port)
          return { ok: true }
        }),
    }),
  }
}

const post = (body: BodyInit, signal?: AbortSignal): RequestInit => ({ method: "POST", body, signal })

describe("OwalletRevive.fetch", () => {
  test("restarts a stopped owallet and sends the request again", async () => {
    const wallet = owallet()
    const fetch = OwalletRevive.make(wallet.deps())
    wallet.stop()
    const response = await fetch(`${wallet.base}/v1/chat/completions`, post("hello"))
    expect(await response.json()).toEqual({ echoed: "hello" })
    expect(wallet.state.starts).toBe(1)
    expect(wallet.state.revived).toBe(1)
    wallet.stop()
  })

  test("leaves a working owallet alone", async () => {
    const wallet = owallet()
    const fetch = OwalletRevive.make(wallet.deps())
    expect(await (await fetch(`${wallet.base}/v1/models`)).json()).toEqual({ echoed: "" })
    expect(wallet.state.starts).toBe(0)
    expect(wallet.state.revived).toBe(0)
    wallet.stop()
  })

  test("requests that fail together share one restart", async () => {
    const wallet = owallet()
    const fetch = OwalletRevive.make(wallet.deps())
    wallet.stop()
    const responses = await Promise.all(["a", "b", "c"].map((body) => fetch(`${wallet.base}/v1/x`, post(body))))
    expect(await Promise.all(responses.map((response) => response.json()))).toEqual([
      { echoed: "a" },
      { echoed: "b" },
      { echoed: "c" },
    ])
    expect(wallet.state.starts).toBe(1)
    wallet.stop()
  })

  test("restarts again when owallet stops a second time", async () => {
    const wallet = owallet()
    const fetch = OwalletRevive.make(wallet.deps())
    wallet.stop()
    await fetch(`${wallet.base}/v1/x`, post("one"))
    wallet.stop()
    expect(await (await fetch(`${wallet.base}/v1/x`, post("two"))).json()).toEqual({ echoed: "two" })
    expect(wallet.state.starts).toBe(2)
    wallet.stop()
  })

  for (const reason of ["no-password", "no-binary", "no-wallet", "start-failed"] as const) {
    test(`says what to do when owallet cannot be restarted (${reason})`, async () => {
      const wallet = owallet()
      const fetch = OwalletRevive.make(wallet.deps(async () => ({ ok: false, reason })))
      wallet.stop()
      const error = await fetch(`${wallet.base}/v1/x`, post("hello")).catch((error) => error)
      expect(error).toBeInstanceOf(OwalletDownError)
      expect(error.message).toBe(OwalletRevive.message(reason))
      expect(error.message).toStartWith("owallet has stopped")
      expect(wallet.state.revived).toBe(0)
      // Shown as written, and not retried: retrying cannot fix it.
      const shown = MessageV2.fromError(error, { providerID: Norm.PROVIDER_ID as never })
      expect(shown).toMatchObject({ data: { message: error.message } })
      expect(SessionRetry.retryable(shown as never, Norm.PROVIDER_ID)).toBeUndefined()
    })
  }

  test("a remote owallet that is down keeps the original error", async () => {
    const wallet = owallet()
    const fetch = OwalletRevive.make(wallet.deps(async () => ({ ok: false, reason: "remote" })))
    wallet.stop()
    const error = await fetch(`${wallet.base}/v1/x`, post("hello")).catch((error) => error)
    expect(error).not.toBeInstanceOf(OwalletDownError)
  })

  test("does not restart owallet for a request the caller aborted", async () => {
    const wallet = owallet()
    const fetch = OwalletRevive.make(wallet.deps())
    wallet.stop()
    const error = await fetch(`${wallet.base}/v1/x`, post("hello", AbortSignal.abort())).catch((error) => error)
    expect(error.name).toBe("AbortError")
    expect(wallet.state.starts).toBe(0)
  })

  test("does not restart owallet for a request to another server", async () => {
    const wallet = owallet()
    const other = owallet()
    const fetch = OwalletRevive.make(wallet.deps())
    wallet.stop()
    other.stop()
    await fetch(`${other.base}/v1/x`, post("hello")).catch(() => {})
    expect(wallet.state.starts).toBe(0)
  })

  test("does not restart owallet for an error while it is up", async () => {
    const wallet = owallet()
    const failure = new TypeError("fetch failed")
    const fetch = OwalletRevive.make({
      ...wallet.deps(),
      fetch: (() => Promise.reject(failure)) as unknown as typeof globalThis.fetch,
    })
    expect(await fetch(`${wallet.base}/v1/x`, post("hello")).catch((error) => error)).toBe(failure)
    expect(wallet.state.starts).toBe(0)
    wallet.stop()
  })

  test("restarts owallet but does not resend a body it can no longer read", async () => {
    const wallet = owallet()
    const fetch = OwalletRevive.make(wallet.deps())
    wallet.stop()
    const body = new Blob(["hello"]).stream()
    const error = await fetch(`${wallet.base}/v1/x`, { ...post(body), duplex: "half" } as RequestInit).catch(
      (error) => error,
    )
    expect(error).not.toBeInstanceOf(OwalletDownError)
    expect(error).toBeInstanceOf(Error)
    expect(wallet.state.starts).toBe(1)
    wallet.stop()
  })
})
