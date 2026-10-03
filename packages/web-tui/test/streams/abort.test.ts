import { describe, expect, test } from "bun:test"
import { abortable, followAbort } from "../../src/abort"

describe("aborts reach owallet-web", () => {
  test("a pending request rejects as soon as its signal aborts", async () => {
    const controller = new AbortController()
    const never = new Promise<Response>(() => {})
    const pending = abortable(never, controller.signal)
    controller.abort(new DOMException("stopped", "AbortError"))
    await expect(pending).rejects.toThrow("stopped")
    await expect(abortable(Promise.resolve(1), AbortSignal.abort())).rejects.toBeDefined()
  })

  test("aborting cancels the streamed body behind the response", async () => {
    let cancelled: unknown
    const body = new ReadableStream<Uint8Array>({
      pull: (c) => c.enqueue(new TextEncoder().encode("data: chunk\n\n")),
      cancel: (reason) => {
        cancelled = reason ?? "cancelled"
      },
    })
    const controller = new AbortController()
    // Runs without the happy-dom preload (test/streams): its Response and
    // TransformStream stand-ins cannot carry a stream.
    const response = followAbort(
      new Response(body, { headers: { "content-type": "text/event-stream" } }),
      controller.signal,
    )
    expect(response.headers.get("content-type")).toBe("text/event-stream")
    const reader = response.body!.getReader()
    expect(new TextDecoder().decode((await reader.read()).value)).toContain("chunk")
    controller.abort()
    await new Promise((resolve) => setTimeout(resolve, 10))
    expect(cancelled).toBeDefined()
  })

  test("a body-less response passes through untouched", () => {
    const response = new Response(null, { status: 204 })
    expect(followAbort(response, new AbortController().signal)).toBe(response)
  })
})
