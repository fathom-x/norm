import { describe, expect, test } from "bun:test"
import type { SandboxListOpts, SandboxOpts } from "e2b"
import type { E2BConfig } from "../../server/config"
import { APP, E2BProvider, PID_FILE, type E2BPtyHandle, type E2BSandbox, type E2BSandboxInfo, type E2BSdk } from "../../server/providers/e2b"
import { LimitError } from "../../server/provider"
import { until } from "./fakes"

const SID = "B".repeat(22)

class FakePty {
  nextPid = 100
  alive = new Set<number>()
  listeners = new Map<number, (data: Uint8Array) => void>()
  exits = new Map<number, { resolve: (v: { exitCode: number }) => void; reject: (e: unknown) => void }>()
  inputs: Array<[number, string]> = []
  resizes: Array<[number, number, number]> = []
  created: Array<Record<string, unknown>> = []
  connected: number[] = []
  disconnects = 0

  private handle(pid: number): E2BPtyHandle {
    return {
      pid,
      wait: () => new Promise((resolve, reject) => this.exits.set(pid, { resolve, reject })),
      disconnect: async () => {
        this.disconnects += 1
        this.listeners.delete(pid)
      },
    }
  }

  readonly api: E2BSandbox["pty"] = {
    create: async (opts) => {
      const pid = this.nextPid++
      this.created.push({ ...opts, onData: undefined })
      this.alive.add(pid)
      this.listeners.set(pid, opts.onData)
      return this.handle(pid)
    },
    connect: async (pid, opts) => {
      if (!this.alive.has(pid)) throw new Error(`process ${pid} not found`)
      this.connected.push(pid)
      if (opts) this.listeners.set(pid, opts.onData)
      return this.handle(pid)
    },
    sendInput: async (pid, data) => void this.inputs.push([pid, new TextDecoder().decode(data)]),
    resize: async (pid, size) => void this.resizes.push([pid, size.cols, size.rows]),
  }
}

class FakeSandbox implements E2BSandbox {
  startedAt = new Date()
  files_ = new Map<string, string>()
  paused = false
  timeouts: number[] = []
  readonly ptyFake = new FakePty()
  readonly pty = this.ptyFake.api
  readonly files = {
    read: async (path: string) => {
      const value = this.files_.get(path)
      if (value === undefined) throw new Error(`${path}: not found`)
      return value
    },
    write: async (path: string, data: string) => void this.files_.set(path, data),
  }
  constructor(
    readonly sandboxId: string,
    public metadata: Record<string, string>,
  ) {}
  async setTimeout(ms: number) {
    this.timeouts.push(ms)
  }
  async pause() {
    this.paused = true
    return true
  }
}

class FakeSdk implements E2BSdk {
  sandboxes = new Map<string, FakeSandbox>()
  createCalls: Array<{ template: string; opts: SandboxOpts }> = []
  connectCalls: Array<{ id: string; timeoutMs?: number }> = []
  listCalls: SandboxListOpts[] = []
  killed: string[] = []
  pausedById: string[] = []
  next = 1

  async create(template: string, opts: SandboxOpts) {
    this.createCalls.push({ template, opts })
    const sandbox = new FakeSandbox(`sbx-${this.next++}`, opts.metadata ?? {})
    this.sandboxes.set(sandbox.sandboxId, sandbox)
    return sandbox
  }
  async connect(id: string, opts: { timeoutMs?: number }) {
    this.connectCalls.push({ id, timeoutMs: opts.timeoutMs })
    const sandbox = this.sandboxes.get(id)
    if (!sandbox) throw new Error("sandbox not found")
    sandbox.paused = false
    return sandbox
  }
  list(opts: SandboxListOpts) {
    this.listCalls.push(opts)
    const query = opts.query ?? {}
    const states = query.state ?? ["running", "paused"]
    const items: E2BSandboxInfo[] = [...this.sandboxes.values()]
      .filter((s) => Object.entries(query.metadata ?? {}).every(([k, v]) => s.metadata[k] === v))
      .map((s) => ({
        sandboxId: s.sandboxId,
        metadata: s.metadata,
        startedAt: s.startedAt,
        state: s.paused ? ("paused" as const) : ("running" as const),
      }))
      .filter((info) => states.includes(info.state))
    let hasNext = true
    return {
      get hasNext() {
        return hasNext
      },
      nextItems: async () => {
        hasNext = false
        return items
      },
    }
  }
  async kill(id: string) {
    this.killed.push(id)
    return this.sandboxes.delete(id)
  }
  async pause(id: string) {
    this.pausedById.push(id)
    const sandbox = this.sandboxes.get(id)
    if (sandbox) sandbox.paused = true
    return true
  }
}

const CONFIG: E2BConfig = {
  apiKey: "e2b_test",
  template: "norm-demo",
  timeoutMs: 900_000,
  overpayHosts: ["overpay-eykm.onrender.com"],
  ptyCommand: "norm-demo",
  ptyCwd: "/home/user",
  domain: undefined,
}

function setup(config: Partial<E2BConfig> = {}) {
  const sdk = new FakeSdk()
  const logs: string[] = []
  const provider = new E2BProvider({ ...CONFIG, ...config, normOwalletEnv: "staging", sdk, log: (m) => logs.push(m) })
  return { sdk, provider, logs }
}

const noAdmit = () => {}
const attachOpts = (data: string[] = [], exits: Array<number | undefined> = []) => ({
  cols: 80,
  rows: 24,
  onData: (bytes: Uint8Array) => void data.push(new TextDecoder().decode(bytes)),
  onExit: (code: number | undefined) => void exits.push(code),
})

describe("e2b provider", () => {
  test("creates a sandbox with the demo's options", async () => {
    const { sdk, provider } = setup()
    const phases: string[] = []
    let admitted = 0
    await provider.findOrCreate(SID, { cols: 80, rows: 24, admit: () => void admitted++, onPhase: (p) => phases.push(p) })
    expect(admitted).toBe(1)
    expect(phases).toEqual(["creating"])
    expect(sdk.createCalls).toHaveLength(1)
    const { template, opts } = sdk.createCalls[0]
    expect(template).toBe("norm-demo")
    expect(opts.apiKey).toBe("e2b_test")
    expect(opts.timeoutMs).toBe(900_000)
    expect(opts.metadata).toEqual({ app: APP, sid: SID })
    expect(opts.lifecycle).toEqual({ onTimeout: "pause" })
    expect(opts.network).toEqual({ allowOut: ["overpay-eykm.onrender.com"], denyOut: ["0.0.0.0/0"] })
    expect(opts.envs?.TERM).toBe("xterm-256color")
    expect(opts.envs?.COLORTERM).toBe("truecolor")
    expect(opts.envs?.NORM_OWALLET_ENV).toBe("staging")
    expect(opts.envs?.OWALLET_PASSWORD).toMatch(/^[A-Za-z0-9_-]{32}$/)
    // A different password for the next sandbox.
    expect(provider.createOptions("C".repeat(22)).envs?.OWALLET_PASSWORD).not.toBe(opts.envs?.OWALLET_PASSWORD)
    // The lookup filtered by app + sid.
    expect(sdk.listCalls[0].query).toEqual({ metadata: { app: APP, sid: SID }, state: ["running", "paused"] })
  })

  test("a refusal from admit creates nothing", async () => {
    const { sdk, provider } = setup()
    const admit = () => {
      throw new LimitError("busy")
    }
    await expect(provider.findOrCreate(SID, { cols: 80, rows: 24, admit })).rejects.toThrow("busy")
    expect(sdk.createCalls).toHaveLength(0)
  })

  test("first attach starts the PTY, stores its pid and execs the demo", async () => {
    const { sdk, provider } = setup()
    const handle = await provider.findOrCreate(SID, { cols: 80, rows: 24, admit: noAdmit })
    const data: string[] = []
    const attachment = await handle.attach({ ...attachOpts(data), cols: 100, rows: 30 })
    const sandbox = [...sdk.sandboxes.values()][0]
    expect(sandbox.ptyFake.created).toEqual([
      {
        cols: 100,
        rows: 30,
        cwd: "/home/user",
        envs: { TERM: "xterm-256color", COLORTERM: "truecolor" },
        onData: undefined,
        timeoutMs: 0,
      },
    ])
    expect(sandbox.files_.get(PID_FILE)).toBe("100")
    expect(sandbox.ptyFake.inputs).toEqual([[100, " exec norm-demo\n"]])

    sandbox.ptyFake.listeners.get(100)!(new TextEncoder().encode("norm>"))
    expect(data).toEqual(["norm>"])
    attachment.write(new TextEncoder().encode("hi"))
    attachment.resize(120, 40)
    await until(() => sandbox.ptyFake.resizes.length === 1)
    expect(sandbox.ptyFake.inputs.at(-1)).toEqual([100, "hi"])
    expect(sandbox.ptyFake.resizes).toEqual([[100, 120, 40]])
  })

  test("no exec line when E2B_PTY_COMMAND is empty", async () => {
    const { sdk, provider } = setup({ ptyCommand: "" })
    await (await provider.findOrCreate(SID, { cols: 80, rows: 24, admit: noAdmit })).attach(attachOpts())
    expect([...sdk.sandboxes.values()][0].ptyFake.inputs).toEqual([])
  })

  test("a later attach reconnects to the stored pid and nudges a redraw", async () => {
    const { sdk, provider } = setup()
    const first = await (await provider.findOrCreate(SID, { cols: 80, rows: 24, admit: noAdmit })).attach(attachOpts())
    first.detach()
    const sandbox = [...sdk.sandboxes.values()][0]
    expect(sandbox.ptyFake.disconnects).toBe(1)

    const data: string[] = []
    await (await provider.findOrCreate(SID, { cols: 80, rows: 24, admit: noAdmit })).attach(attachOpts(data))
    expect(sandbox.ptyFake.created).toHaveLength(1)
    expect(sandbox.ptyFake.connected).toEqual([100])
    expect(sandbox.ptyFake.resizes).toEqual([
      [100, 80, 23],
      [100, 80, 24],
    ])
    sandbox.ptyFake.listeners.get(100)!(new TextEncoder().encode("redraw"))
    expect(data).toEqual(["redraw"])
  })

  test("a dead pid starts a new PTY", async () => {
    const { sdk, provider } = setup()
    await (await provider.findOrCreate(SID, { cols: 80, rows: 24, admit: noAdmit })).attach(attachOpts())
    const sandbox = [...sdk.sandboxes.values()][0]
    sandbox.ptyFake.alive.clear()
    await (await provider.findOrCreate(SID, { cols: 80, rows: 24, admit: noAdmit })).attach(attachOpts())
    expect(sandbox.ptyFake.created).toHaveLength(2)
    expect(sandbox.files_.get(PID_FILE)).toBe("101")
  })

  test("a paused sandbox is found by metadata and resumed with connect (new broker process)", async () => {
    const { sdk, provider } = setup()
    await (await provider.findOrCreate(SID, { cols: 80, rows: 24, admit: noAdmit })).attach(attachOpts())
    await provider.pause(SID)
    const sandbox = [...sdk.sandboxes.values()][0]
    expect(sandbox.paused).toBe(true)

    // A restarted broker: same SDK state, empty caches.
    const restarted = new E2BProvider({ ...CONFIG, normOwalletEnv: "staging", sdk, log: () => {} })
    expect(await restarted.status(SID)).toBe("paused")
    const phases: string[] = []
    let admitted = 0
    const handle = await restarted.findOrCreate(SID, {
      cols: 80,
      rows: 24,
      admit: () => void admitted++,
      onPhase: (p) => phases.push(p),
    })
    expect(admitted).toBe(0)
    expect(phases).toEqual(["resuming"])
    expect(sdk.connectCalls.at(-1)).toEqual({ id: sandbox.sandboxId, timeoutMs: 900_000 })
    expect(sdk.createCalls).toHaveLength(1)
    await handle.attach(attachOpts())
    expect(sandbox.ptyFake.connected).toEqual([100])
  })

  test("a cached sandbox is reconnected (resumed if it auto-paused) without a list", async () => {
    const { sdk, provider } = setup()
    await provider.findOrCreate(SID, { cols: 80, rows: 24, admit: noAdmit })
    const lists = sdk.listCalls.length
    await provider.findOrCreate(SID, { cols: 80, rows: 24, admit: noAdmit })
    expect(sdk.listCalls.length).toBe(lists)
    expect(sdk.connectCalls).toHaveLength(1)
  })

  test("two tabs at once share one creation", async () => {
    const { sdk, provider } = setup()
    await Promise.all([
      provider.findOrCreate(SID, { cols: 80, rows: 24, admit: noAdmit }),
      provider.findOrCreate(SID, { cols: 80, rows: 24, admit: noAdmit }),
    ])
    expect(sdk.createCalls).toHaveLength(1)
  })

  test("exit codes: CommandExitError's code is an exit, a broken stream is not", async () => {
    const { sdk, provider } = setup()
    const exits: Array<number | undefined> = []
    await (await provider.findOrCreate(SID, { cols: 80, rows: 24, admit: noAdmit })).attach(attachOpts([], exits))
    const pty = [...sdk.sandboxes.values()][0].ptyFake
    pty.exits.get(100)!.reject(Object.assign(new Error("exit status 2"), { exitCode: 2 }))
    await until(() => exits.length === 1)
    expect(exits).toEqual([2])

    const exits2: Array<number | undefined> = []
    pty.alive.add(100)
    await (await provider.findOrCreate(SID, { cols: 80, rows: 24, admit: noAdmit })).attach(attachOpts([], exits2))
    pty.exits.get(100)!.reject(new Error("stream reset"))
    await until(() => exits2.length === 1)
    expect(exits2).toEqual([undefined])
  })

  test("after detach, output and exits are ignored", async () => {
    const { sdk, provider } = setup()
    const data: string[] = []
    const exits: Array<number | undefined> = []
    const attachment = await (await provider.findOrCreate(SID, { cols: 80, rows: 24, admit: noAdmit })).attach(
      attachOpts(data, exits),
    )
    const pty = [...sdk.sandboxes.values()][0].ptyFake
    const listener = pty.listeners.get(100)!
    attachment.detach()
    listener(new TextEncoder().encode("late"))
    pty.exits.get(100)!.resolve({ exitCode: 0 })
    await Bun.sleep(5)
    expect(data).toEqual([])
    expect(exits).toEqual([])
  })

  test("input extends the sandbox timeout at most every third of it", async () => {
    const { sdk, provider } = setup({ timeoutMs: 60_000 })
    const attachment = await (await provider.findOrCreate(SID, { cols: 80, rows: 24, admit: noAdmit })).attach(attachOpts())
    const sandbox = [...sdk.sandboxes.values()][0]
    const realNow = Date.now
    try {
      Date.now = () => realNow() + 25_000
      attachment.write(new Uint8Array([97]))
      attachment.write(new Uint8Array([98]))
      await until(() => sandbox.ptyFake.inputs.length >= 3)
    } finally {
      Date.now = realNow
    }
    expect(sandbox.timeouts).toEqual([60_000])
  })

  test("reset kills every sandbox of the visitor; count counts running demo sandboxes", async () => {
    const { sdk, provider } = setup()
    await provider.findOrCreate(SID, { cols: 80, rows: 24, admit: noAdmit })
    await provider.findOrCreate("D".repeat(22), { cols: 80, rows: 24, admit: noAdmit })
    await sdk.create("other", { metadata: { app: "something-else" } })
    expect(await provider.count()).toBe(2)
    expect(sdk.listCalls.at(-1)?.query).toEqual({ metadata: { app: APP }, state: ["running"] })
    await provider.pause("D".repeat(22))
    expect(await provider.count()).toBe(1)

    await provider.reset(SID)
    expect(sdk.killed).toEqual(["sbx-1"])
    expect(await provider.status(SID)).toBe("none")
    // Next visit creates afresh.
    await provider.findOrCreate(SID, { cols: 80, rows: 24, admit: noAdmit })
    expect(sdk.createCalls.filter((c) => c.opts.metadata?.sid === SID)).toHaveLength(2)
  })

  test("the sweeper deletes only this app's paused sandboxes past retention", async () => {
    const { sdk, provider } = setup()
    const DAY = 86_400_000
    await provider.findOrCreate(SID, { cols: 80, rows: 24, admit: noAdmit }) // old, paused → swept
    await provider.findOrCreate("D".repeat(22), { cols: 80, rows: 24, admit: noAdmit }) // recent, paused → kept
    await provider.findOrCreate("E".repeat(22), { cols: 80, rows: 24, admit: noAdmit }) // old, running → kept
    await sdk.create("other", { metadata: { app: "something-else" } }) // not ours → kept
    sdk.sandboxes.get("sbx-1")!.startedAt = new Date(Date.now() - 8 * DAY)
    sdk.sandboxes.get("sbx-3")!.startedAt = new Date(Date.now() - 8 * DAY)
    sdk.sandboxes.get("sbx-4")!.startedAt = new Date(Date.now() - 8 * DAY)
    sdk.sandboxes.get("sbx-4")!.paused = true
    await provider.pause(SID)
    await provider.pause("D".repeat(22))

    expect(await provider.sweep(7 * DAY)).toBe(1)
    expect(sdk.killed).toEqual(["sbx-1"])
    expect(sdk.listCalls.at(-1)?.query).toEqual({ metadata: { app: APP }, state: ["paused"] })
  })

  test("pause without a cached instance pauses by id", async () => {
    const { sdk, provider } = setup()
    await provider.findOrCreate(SID, { cols: 80, rows: 24, admit: noAdmit })
    const restarted = new E2BProvider({ ...CONFIG, normOwalletEnv: "staging", sdk, log: () => {} })
    await restarted.pause(SID)
    expect(sdk.pausedById).toEqual(["sbx-1"])
  })
})
