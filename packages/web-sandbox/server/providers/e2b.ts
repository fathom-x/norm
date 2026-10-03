// Production provider: one E2B sandbox per visitor (template `norm-demo`),
// found again by its metadata `{app: "norm-demo", sid}` — no database.
//
// - create: auto-pause on timeout (lifecycle.onTimeout "pause"), an egress
//   allowlist of the Overpay host(s) only, a random owallet password that
//   never leaves the sandbox;
// - resume: `Sandbox.connect(id)` restores a paused sandbox, processes and all;
// - terminal: a PTY (the template user's login shell, told to `exec
//   norm-demo`), whose pid is kept in a file inside the sandbox so a later
//   visit — even from a restarted broker — reattaches with `pty.connect`;
// - pause/reset/count: `Sandbox.pause` / `Sandbox.kill` / `Sandbox.list`.
//
// The SDK is injected (`E2BSdk`) so the logic is tested against a fake;
// `realSdk()` binds the `e2b` package.
import { randomBytes } from "node:crypto"
import type { SandboxApiOpts, SandboxConnectOpts, SandboxInfo, SandboxListOpts, SandboxOpts } from "e2b"
import type { E2BConfig } from "../config"
import type { AttachOptions, Attachment, FindOrCreateOptions, SandboxHandle, SandboxProvider, SandboxState } from "../provider"

export const APP = "norm-demo"
/** Where the terminal's pid is kept inside the sandbox. */
export const PID_FILE = "/home/user/.norm-demo/pty.pid"

/** The slice of a PTY `CommandHandle` this provider uses. */
export interface E2BPtyHandle {
  readonly pid: number
  wait(): Promise<{ exitCode: number }>
  disconnect(): Promise<void>
}

/** The slice of an e2b `Sandbox` instance this provider uses. */
export interface E2BSandbox {
  readonly sandboxId: string
  readonly pty: {
    create(opts: {
      cols: number
      rows: number
      onData: (data: Uint8Array) => void
      timeoutMs?: number
      envs?: Record<string, string>
      cwd?: string
    }): Promise<E2BPtyHandle>
    connect(pid: number, opts?: { onData: (data: Uint8Array) => void; timeoutMs?: number }): Promise<E2BPtyHandle>
    sendInput(pid: number, data: Uint8Array): Promise<void>
    resize(pid: number, size: { cols: number; rows: number }): Promise<void>
  }
  readonly files: {
    read(path: string): Promise<string>
    write(path: string, data: string): Promise<unknown>
  }
  setTimeout(timeoutMs: number): Promise<void>
  pause(): Promise<boolean>
}

export type E2BSandboxInfo = Pick<SandboxInfo, "sandboxId" | "state" | "metadata"> &
  Partial<Pick<SandboxInfo, "startedAt">>

/** The slice of the e2b `Sandbox` class (static API) this provider uses. */
export interface E2BSdk {
  create(template: string, opts: SandboxOpts): Promise<E2BSandbox>
  connect(sandboxId: string, opts: SandboxConnectOpts): Promise<E2BSandbox>
  list(opts: SandboxListOpts): { readonly hasNext: boolean; nextItems(): Promise<E2BSandboxInfo[]> }
  kill(sandboxId: string, opts: SandboxApiOpts): Promise<boolean>
  pause(sandboxId: string, opts: SandboxApiOpts): Promise<boolean>
}

export async function realSdk(): Promise<E2BSdk> {
  const { Sandbox } = await import("e2b")
  return {
    create: (template, opts) => Sandbox.create(template, opts),
    connect: (sandboxId, opts) => Sandbox.connect(sandboxId, opts),
    list: (opts) => Sandbox.list(opts),
    kill: (sandboxId, opts) => Sandbox.kill(sandboxId, opts),
    pause: (sandboxId, opts) => Sandbox.pause(sandboxId, opts),
  }
}

export interface E2BProviderOptions extends E2BConfig {
  normOwalletEnv: string
  sdk?: E2BSdk
  log?: (message: string) => void
}

const encoder = new TextEncoder()

export class E2BProvider implements SandboxProvider {
  readonly name = "e2b"
  private sdkPromise: Promise<E2BSdk> | undefined
  /** Connected sandboxes this process holds, by sid (dropped on pause/reset). */
  private readonly live = new Map<string, E2BSandbox>()
  /** In-flight findOrCreate per sid: two tabs at once must not create two sandboxes. */
  private readonly pending = new Map<string, Promise<E2BSandbox>>()

  constructor(private readonly options: E2BProviderOptions) {
    if (options.sdk) this.sdkPromise = Promise.resolve(options.sdk)
  }

  private sdk() {
    return (this.sdkPromise ??= realSdk())
  }

  private get api(): SandboxApiOpts {
    return { apiKey: this.options.apiKey, ...(this.options.domain && { domain: this.options.domain }) }
  }

  private log(message: string) {
    ;(this.options.log ?? console.log)(`[e2b] ${message}`)
  }

  /** The visitor's sandboxes (normally one), running ones first. */
  private async find(sid: string): Promise<E2BSandboxInfo[]> {
    const sdk = await this.sdk()
    const paginator = sdk.list({
      ...this.api,
      query: { metadata: { app: APP, sid }, state: ["running", "paused"] },
      limit: 10,
    })
    const found = paginator.hasNext ? await paginator.nextItems() : []
    return found.sort((a, b) => Number(b.state === "running") - Number(a.state === "running"))
  }

  createOptions(sid: string): SandboxOpts {
    const { overpayHosts } = this.options
    return {
      ...this.api,
      timeoutMs: this.options.timeoutMs,
      metadata: { app: APP, sid },
      envs: {
        // Stays in the sandbox: the visitor never needs it (norm's first run
        // reads it instead of prompting).
        OWALLET_PASSWORD: randomBytes(24).toString("base64url"),
        TERM: "xterm-256color",
        COLORTERM: "truecolor",
        NORM_OWALLET_ENV: this.options.normOwalletEnv,
      },
      // Auto-pause (keeping memory) instead of dying at the timeout.
      lifecycle: { onTimeout: "pause" },
      // The visitor has a real shell: it may reach Overpay and nothing else.
      network: { allowOut: overpayHosts, denyOut: ["0.0.0.0/0"] },
    }
  }

  async findOrCreate(sid: string, options: FindOrCreateOptions): Promise<SandboxHandle> {
    let pending = this.pending.get(sid)
    if (!pending) {
      pending = this.resolve(sid, options).finally(() => this.pending.delete(sid))
      this.pending.set(sid, pending)
    }
    const sandbox = await pending
    return { attach: (attach) => this.attach(sid, sandbox, attach) }
  }

  private async resolve(sid: string, options: FindOrCreateOptions): Promise<E2BSandbox> {
    const sdk = await this.sdk()
    const cached = this.live.get(sid)
    if (cached) {
      // It may have auto-paused since: connect resumes it (or just extends a
      // running one's timeout) without a list call.
      try {
        const sandbox = await sdk.connect(cached.sandboxId, { ...this.api, timeoutMs: this.options.timeoutMs })
        this.live.set(sid, sandbox)
        return sandbox
      } catch (error) {
        this.log(`${cached.sandboxId} is gone (${(error as Error).message})`)
        this.live.delete(sid)
      }
    }
    const [existing] = await this.find(sid)
    let sandbox: E2BSandbox
    if (existing) {
      if (existing.state === "paused") options.onPhase?.("resuming")
      // Resumes a paused sandbox; for a running one, extends its timeout.
      sandbox = await sdk.connect(existing.sandboxId, { ...this.api, timeoutMs: this.options.timeoutMs })
    } else {
      await options.admit()
      options.onPhase?.("creating")
      sandbox = await sdk.create(this.options.template, this.createOptions(sid))
      this.log(`created ${sandbox.sandboxId}`)
    }
    this.live.set(sid, sandbox)
    return sandbox
  }

  private async readPid(sandbox: E2BSandbox): Promise<number | undefined> {
    try {
      const pid = Number((await sandbox.files.read(PID_FILE)).trim())
      return Number.isInteger(pid) && pid > 0 ? pid : undefined
    } catch {
      return undefined
    }
  }

  private async attach(sid: string, sandbox: E2BSandbox, options: AttachOptions): Promise<Attachment> {
    let detached = false
    const onData = (data: Uint8Array) => {
      if (!detached) options.onData(data)
    }
    const { cols, rows } = options

    let handle: E2BPtyHandle | undefined
    const pid = await this.readPid(sandbox)
    if (pid !== undefined) {
      try {
        handle = await sandbox.pty.connect(pid, { onData, timeoutMs: 0 })
        // Same size → no SIGWINCH → no redraw: nudge it.
        await sandbox.pty.resize(pid, { cols, rows: rows > 2 ? rows - 1 : rows + 1 })
        await sandbox.pty.resize(pid, { cols, rows })
      } catch (error) {
        this.log(`pty ${pid} in ${sandbox.sandboxId} is gone (${(error as Error).message}); starting a new one`)
        handle = undefined
      }
    }
    if (!handle) {
      handle = await sandbox.pty.create({
        cols,
        rows,
        cwd: this.options.ptyCwd,
        envs: { TERM: "xterm-256color", COLORTERM: "truecolor" },
        onData,
        // The terminal lives as long as the sandbox, not 60 s.
        timeoutMs: 0,
      })
      await sandbox.files.write(PID_FILE, String(handle.pid))
      // The PTY runs the user's login shell; replace it with the demo (the
      // leading space keeps the line out of the shell's history).
      if (this.options.ptyCommand)
        await sandbox.pty.sendInput(handle.pid, encoder.encode(` exec ${this.options.ptyCommand}\n`))
    }

    const current = handle
    current.wait().then(
      (result) => !detached && options.onExit(result.exitCode),
      (error: unknown) => {
        if (detached) return
        // CommandExitError carries the exit code; anything else is a broken
        // stream (e.g. the sandbox auto-paused), not an exit.
        const code = (error as { exitCode?: unknown }).exitCode
        options.onExit(typeof code === "number" ? code : undefined)
      },
    )

    // Input keeps the sandbox awake: extend its timeout at most every third
    // of it. An idle tab lets it auto-pause.
    let extendedAt = Date.now()
    let queue = Promise.resolve()
    const run = (what: string, task: () => Promise<unknown>) => {
      queue = queue.then(task).then(
        () => undefined,
        (error: unknown) => this.log(`${what} failed for ${sid}: ${(error as Error).message}`),
      )
    }
    return {
      write: (bytes) => {
        if (detached) return
        run("input", () => sandbox.pty.sendInput(current.pid, bytes))
        if (Date.now() - extendedAt > this.options.timeoutMs / 3) {
          extendedAt = Date.now()
          run("keepalive", () => sandbox.setTimeout(this.options.timeoutMs))
        }
      },
      resize: (cols, rows) => {
        if (!detached) run("resize", () => sandbox.pty.resize(current.pid, { cols, rows }))
      },
      detach: () => {
        if (detached) return
        detached = true
        current.disconnect().catch(() => {})
      },
    }
  }

  async reset(sid: string) {
    const sdk = await this.sdk()
    this.live.delete(sid)
    for (const info of await this.find(sid)) {
      await sdk.kill(info.sandboxId, this.api)
      this.log(`killed ${info.sandboxId}`)
    }
  }

  async pause(sid: string) {
    const sandbox = this.live.get(sid)
    this.live.delete(sid)
    if (sandbox) {
      await sandbox.pause()
      return
    }
    const sdk = await this.sdk()
    for (const info of await this.find(sid)) if (info.state === "running") await sdk.pause(info.sandboxId, this.api)
  }

  async status(sid: string): Promise<SandboxState> {
    const [info] = await this.find(sid)
    return info ? info.state : "none"
  }

  async sweep(maxAgeMs: number) {
    const sdk = await this.sdk()
    const paginator = sdk.list({ ...this.api, query: { metadata: { app: APP }, state: ["paused"] }, limit: 100 })
    const cutoff = Date.now() - maxAgeMs
    let killed = 0
    while (paginator.hasNext)
      for (const info of await paginator.nextItems()) {
        const started = info.startedAt ? new Date(info.startedAt).getTime() : NaN
        if (!(started < cutoff)) continue
        await sdk.kill(info.sandboxId, this.api).catch(() => false)
        killed++
        this.log(`swept ${info.sandboxId} (paused, started ${new Date(started).toISOString()})`)
      }
    return killed
  }

  async count() {
    const sdk = await this.sdk()
    const paginator = sdk.list({ ...this.api, query: { metadata: { app: APP }, state: ["running"] }, limit: 100 })
    let count = 0
    while (paginator.hasNext) count += (await paginator.nextItems()).length
    return count
  }
}
