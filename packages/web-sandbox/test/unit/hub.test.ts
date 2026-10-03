import { describe, expect, test } from "bun:test"
import { admission, CLOSE, Hub, parseControl } from "../../server/hub"
import { LimitError } from "../../server/provider"
import { RateLimiter } from "../../server/rate-limit"
import { FakePeer, FakeProvider, sleep, until } from "./fakes"

const SID = "A".repeat(22)

function setup(options: { pauseGraceMs?: number; admit?: (ip: string) => void | Promise<void> } = {}) {
  const provider = new FakeProvider()
  const logs: string[] = []
  const hub = new Hub({
    provider,
    pauseGraceMs: options.pauseGraceMs ?? 30,
    helloTimeoutMs: 200,
    admit: options.admit ?? (() => {}),
    log: (message) => logs.push(message),
  })
  return { provider, hub, logs }
}

const hello = (cols = 80, rows = 24) => JSON.stringify({ type: "hello", cols, rows })
const bytes = (text: string) => new TextEncoder().encode(text)

describe("control messages", () => {
  test("hello and resize with sane sizes", () => {
    expect(parseControl(hello(100, 30))).toEqual({ type: "hello", size: { cols: 100, rows: 30 } })
    expect(parseControl('{"type":"resize","cols":2,"rows":2}')?.type).toBe("resize")
  })
  test("anything else is refused", () => {
    for (const text of [
      "nope",
      "[]",
      '{"type":"exec"}',
      '{"type":"hello","cols":0,"rows":24}',
      '{"type":"hello","cols":80.5,"rows":24}',
      '{"type":"hello","cols":"80","rows":24}',
      '{"type":"resize","cols":80,"rows":100000}',
      `{"type":"hello","cols":80,"rows":24,"pad":"${"x".repeat(5000)}"}`,
    ])
      expect(parseControl(text)).toBeUndefined()
  })
})

describe("hub", () => {
  test("hello → creating → starting → ready; bytes both ways; resize", async () => {
    const { provider, hub } = setup()
    const peer = new FakePeer()
    const connection = hub.open(peer, SID, "1.2.3.4")
    connection.onBinary(bytes("early")) // before hello: ignored
    connection.onText(hello(90, 30))
    connection.onBinary(bytes("typed-while-starting"))
    await until(() => peer.phases.includes("ready"), 1000, "ready")
    expect(peer.phases).toEqual(["creating", "starting", "ready"])
    const terminal = provider.terminal(SID)
    expect(terminal.sizes[0]).toEqual([90, 30])
    // Input queued while starting is delivered once attached.
    expect(terminal.input.map((b) => new TextDecoder().decode(b))).toEqual(["typed-while-starting"])

    connection.onBinary(bytes("ls\r"))
    expect(new TextDecoder().decode(terminal.input.at(-1))).toBe("ls\r")
    terminal.output("hello from the sandbox")
    expect(peer.output).toBe("hello from the sandbox")

    connection.onText(JSON.stringify({ type: "resize", cols: 120, rows: 40 }))
    connection.onText(JSON.stringify({ type: "resize", cols: 0, rows: 40 })) // invalid: ignored
    expect(terminal.sizes.at(-1)).toEqual([120, 40])
    expect(hub.size).toBe(1)
  })

  test("a paused sandbox reports resuming", async () => {
    const { provider, hub } = setup()
    provider.states.set(SID, "paused")
    const peer = new FakePeer()
    hub.open(peer, SID, "ip").onText(hello())
    await until(() => peer.phases.includes("ready"))
    expect(peer.phases).toEqual(["resuming", "starting", "ready"])
  })

  test("no hello → closed with a protocol error", async () => {
    const { hub } = setup()
    const peer = new FakePeer()
    hub.open(peer, SID, "ip")
    await until(() => !!peer.closed, 1000, "close")
    expect(peer.closed?.code).toBe(CLOSE.protocol)
  })

  test("a bad first message → protocol error", () => {
    const { hub } = setup()
    const peer = new FakePeer()
    hub.open(peer, SID, "ip").onText('{"type":"resize","cols":80,"rows":24}')
    expect(peer.closed?.code).toBe(CLOSE.protocol)
  })

  test("a second connection replaces the first", async () => {
    const { provider, hub } = setup()
    const first = new FakePeer()
    const a = hub.open(first, SID, "ip")
    a.onText(hello())
    await until(() => first.phases.includes("ready"))

    const second = new FakePeer()
    const b = hub.open(second, SID, "ip")
    b.onText(hello(100, 40))
    await until(() => second.phases.includes("ready"))
    expect(first.phases.at(-1)).toBe("replaced")
    expect(first.closed?.code).toBe(CLOSE.replaced)
    expect(a.closed).toBe(true)
    const terminal = provider.terminal(SID)
    expect(terminal.detached).toBe(1)
    // Output now goes to the second peer only.
    terminal.output("x")
    expect(second.output).toBe("x")
    expect(first.output).toBe("")
    expect(hub.size).toBe(1)
    // The replaced socket's close does not schedule a pause.
    a.onClose()
    await sleep(60)
    expect(provider.calls.filter((call) => call.startsWith("pause"))).toEqual([])
  })

  test("disconnect → pause after the grace period, unless the visitor is back", async () => {
    const { provider, hub } = setup({ pauseGraceMs: 40 })
    const peer = new FakePeer()
    const a = hub.open(peer, SID, "ip")
    a.onText(hello())
    await until(() => peer.phases.includes("ready"))
    a.onClose()
    expect(provider.terminal(SID).detached).toBe(1)
    // Back within the grace period: no pause.
    await sleep(10)
    const again = new FakePeer()
    const b = hub.open(again, SID, "ip")
    b.onText(hello())
    await until(() => again.phases.includes("ready"))
    await sleep(60)
    expect(provider.calls).not.toContain(`pause ${SID}`)
    // Gone for good: paused.
    b.onClose()
    await until(() => provider.calls.includes(`pause ${SID}`), 1000, "pause")
    expect(provider.states.get(SID)).toBe("paused")
  })

  test("the program exiting → exited status with its code, then close", async () => {
    const { provider, hub } = setup()
    const peer = new FakePeer()
    hub.open(peer, SID, "ip").onText(hello())
    await until(() => peer.phases.includes("ready"))
    provider.terminal(SID).exit(3)
    expect(peer.statuses.at(-1)).toEqual({ type: "status", phase: "exited", code: 3 })
    expect(peer.closed?.code).toBe(CLOSE.exited)
  })

  test("a broken stream (no exit code) → error, not exited", async () => {
    const { provider, hub } = setup()
    const peer = new FakePeer()
    hub.open(peer, SID, "ip").onText(hello())
    await until(() => peer.phases.includes("ready"))
    provider.terminal(SID).exit(undefined)
    expect(peer.phases.at(-1)).toBe("error")
    expect(peer.closed?.code).toBe(CLOSE.error)
  })

  test("a refusal from admit is shown as-is", async () => {
    const { hub } = setup({
      admit: () => {
        throw new LimitError("All demo sandboxes are busy")
      },
    })
    const peer = new FakePeer()
    hub.open(peer, SID, "ip").onText(hello())
    await until(() => !!peer.closed)
    expect(peer.statuses.at(-1)).toEqual({ type: "status", phase: "error", message: "All demo sandboxes are busy" })
    expect(peer.closed?.code).toBe(CLOSE.error)
  })

  test("a provider failure is logged, not shown", async () => {
    const { provider, hub, logs } = setup()
    provider.failWith = new Error("E2B said no: secret detail")
    const peer = new FakePeer()
    hub.open(peer, SID, "ip").onText(hello())
    await until(() => !!peer.closed)
    expect(peer.statuses.at(-1)?.message).toBe("Could not start your sandbox.")
    expect(logs.join("\n")).toContain("secret detail")
  })

  test("closing while the sandbox is still coming up never attaches", async () => {
    const { provider, hub } = setup()
    provider.delayMs = 30
    const peer = new FakePeer()
    const connection = hub.open(peer, SID, "ip")
    connection.onText(hello())
    connection.onClose()
    await sleep(60)
    expect(provider.terminal(SID).attachments).toHaveLength(0)
  })

  test("reset closes the live connection and deletes the sandbox", async () => {
    const { provider, hub } = setup({ pauseGraceMs: 20 })
    const peer = new FakePeer()
    hub.open(peer, SID, "ip").onText(hello())
    await until(() => peer.phases.includes("ready"))
    await hub.reset(SID)
    expect(peer.closed?.code).toBe(CLOSE.reset)
    expect(provider.calls).toContain(`reset ${SID}`)
    expect(await provider.status(SID)).toBe("none")
    await sleep(40)
    expect(provider.calls).not.toContain(`pause ${SID}`)
  })
})

describe("admission", () => {
  test("refuses at the sandbox cap, then by per-IP rate", async () => {
    const provider = new FakeProvider()
    const admit = admission({ provider, maxSandboxes: 1, limiter: new RateLimiter(2, 3_600_000) })
    await admit("1.1.1.1")
    provider.states.set("other", "running")
    await expect(admit("1.1.1.1")).rejects.toThrow(/busy/)
    provider.states.clear()
    await admit("1.1.1.1")
    await expect(admit("1.1.1.1")).rejects.toThrow(/Too many new sandboxes.*(30|31) min/)
    await admit("2.2.2.2")
  })
})
