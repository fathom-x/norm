// The terminal WebSocket protocol, independent of Bun's socket type (tests
// drive it with fake peers).
//
//   binary frames  terminal bytes, both directions
//   text frames    JSON control
//     client → server  {type:"hello", cols, rows}   first message, required
//                      {type:"resize", cols, rows}
//     server → client  {type:"status", phase, message?, code?}
//                      phase: creating | resuming | starting | ready |
//                             exited | replaced | error
//
// One live connection per visitor: a new one replaces the old (which gets
// `replaced`, then is closed). When the last connection closes, the sandbox is
// paused after `pauseGraceMs` unless the visitor is back by then.
import { LimitError, type Attachment, type SandboxProvider } from "./provider"

export type Phase = "creating" | "resuming" | "starting" | "ready" | "exited" | "replaced" | "error"

export interface StatusMessage {
  type: "status"
  phase: Phase
  message?: string
  code?: number
}

/** Close codes the page acts on (4000–4999 are the application's). */
export const CLOSE = {
  /** Malformed or missing hello. */
  protocol: 4000,
  /** Another tab took over. */
  replaced: 4001,
  /** The visitor reset their sandbox ("Start over"). */
  reset: 4002,
  /** The sandbox could not be started (the status message says why). */
  error: 4003,
  /** The terminal program exited. */
  exited: 4004,
} as const

export const LIMITS = {
  cols: { min: 2, max: 1000 },
  rows: { min: 2, max: 500 },
  /** Largest control (text) frame accepted. */
  textFrameBytes: 4096,
  /** Input held while the terminal is still coming up. */
  pendingInputBytes: 64 * 1024,
}

/** What the hub needs from a socket. */
export interface Peer {
  sendText(text: string): void
  sendBinary(bytes: Uint8Array): void
  close(code: number, reason: string): void
}

export interface HubOptions {
  provider: SandboxProvider
  pauseGraceMs: number
  helloTimeoutMs: number
  /** Before a new sandbox is created: throw LimitError to refuse. */
  admit(ip: string): void | Promise<void>
  log?: (message: string) => void
}

type Size = { cols: number; rows: number }

function validSize(value: { cols?: unknown; rows?: unknown }): Size | undefined {
  const { cols, rows } = value
  if (!Number.isInteger(cols) || !Number.isInteger(rows)) return undefined
  const c = cols as number
  const r = rows as number
  if (c < LIMITS.cols.min || c > LIMITS.cols.max || r < LIMITS.rows.min || r > LIMITS.rows.max) return undefined
  return { cols: c, rows: r }
}

export function parseControl(text: string): { type: "hello" | "resize"; size: Size } | undefined {
  if (text.length > LIMITS.textFrameBytes) return undefined
  let value: unknown
  try {
    value = JSON.parse(text)
  } catch {
    return undefined
  }
  if (!value || typeof value !== "object") return undefined
  const message = value as { type?: unknown; cols?: unknown; rows?: unknown }
  if (message.type !== "hello" && message.type !== "resize") return undefined
  const size = validSize(message)
  return size && { type: message.type, size }
}

export class Connection {
  private state: "hello" | "starting" | "ready" | "closed" = "hello"
  private attachment: Attachment | undefined
  private pending: Uint8Array[] = []
  private pendingBytes = 0
  private size: Size | undefined
  private readonly helloTimer: ReturnType<typeof setTimeout>

  constructor(
    private readonly hub: Hub,
    private readonly peer: Peer,
    readonly sid: string,
    readonly ip: string,
  ) {
    this.helloTimer = setTimeout(() => this.end(CLOSE.protocol, "no hello"), hub.options.helloTimeoutMs)
  }

  get closed() {
    return this.state === "closed"
  }

  status(phase: Phase, extra: { message?: string; code?: number } = {}) {
    if (this.closed) return
    const message: StatusMessage = { type: "status", phase, ...extra }
    this.peer.sendText(JSON.stringify(message))
  }

  onText(text: string) {
    if (this.closed) return
    const control = parseControl(text)
    if (this.state === "hello") {
      if (control?.type !== "hello") return this.end(CLOSE.protocol, "expected hello")
      clearTimeout(this.helloTimer)
      this.size = control.size
      this.state = "starting"
      void this.start(control.size)
      return
    }
    if (control?.type !== "resize") return
    this.size = control.size
    this.attachment?.resize(control.size.cols, control.size.rows)
  }

  onBinary(bytes: Uint8Array) {
    if (this.state === "ready") return this.attachment?.write(bytes)
    if (this.state !== "starting") return
    if (this.pendingBytes + bytes.length > LIMITS.pendingInputBytes) return
    this.pending.push(bytes.slice())
    this.pendingBytes += bytes.length
  }

  private async start(size: Size) {
    this.hub.claim(this)
    // One start at a time per visitor: a replaced connection's start must
    // finish (and see it was closed) before this one looks for a terminal, or
    // both could start one.
    await this.hub.serial(this.sid, () => this.run(size))
  }

  private async run(size: Size) {
    if (this.closed) return
    const { provider } = this.hub.options
    try {
      const handle = await provider.findOrCreate(this.sid, {
        ...size,
        admit: () => this.hub.options.admit(this.ip),
        onPhase: (phase) => this.status(phase),
      })
      if (this.closed) return
      this.status("starting")
      const attachment = await handle.attach({
        ...(this.size ?? size),
        onData: (bytes) => {
          if (!this.closed) this.peer.sendBinary(bytes)
        },
        onExit: (code) => {
          if (this.closed) return
          this.attachment = undefined
          if (code === undefined) {
            this.status("error", { message: "Lost the connection to the sandbox." })
            this.end(CLOSE.error, "lost")
          } else {
            this.status("exited", { code })
            this.end(CLOSE.exited, "exited")
          }
        },
      })
      if (this.closed) {
        attachment.detach()
        return
      }
      this.attachment = attachment
      const size2 = this.size ?? size
      if (size2.cols !== size.cols || size2.rows !== size.rows) attachment.resize(size2.cols, size2.rows)
      for (const bytes of this.pending) attachment.write(bytes)
      this.pending = []
      this.pendingBytes = 0
      this.state = "ready"
      this.status("ready")
    } catch (error) {
      if (this.closed) return
      const limit = error instanceof LimitError
      if (!limit) this.hub.log(`start failed for ${this.sid.slice(0, 6)}…: ${(error as Error).stack ?? error}`)
      this.status("error", { message: limit ? (error as Error).message : "Could not start your sandbox." })
      this.end(CLOSE.error, limit ? "limit" : "error")
    }
  }

  /** Another connection took over this visitor's terminal. */
  replace() {
    this.status("replaced", { message: "Opened in another tab." })
    this.end(CLOSE.replaced, "replaced")
  }

  /** Close from our side. */
  end(code: number, reason: string) {
    if (this.closed) return
    this.peer.close(code, reason)
    this.onClose()
  }

  /** The socket closed (either side). */
  onClose() {
    if (this.closed) return
    this.state = "closed"
    clearTimeout(this.helloTimer)
    this.attachment?.detach()
    this.attachment = undefined
    this.pending = []
    this.hub.release(this)
  }
}

export class Hub {
  private readonly connections = new Map<string, Connection>()
  private readonly graceTimers = new Map<string, ReturnType<typeof setTimeout>>()

  constructor(readonly options: HubOptions) {}

  log(message: string) {
    ;(this.options.log ?? console.error)(message)
  }

  open(peer: Peer, sid: string, ip: string) {
    return new Connection(this, peer, sid, ip)
  }

  private readonly starts = new Map<string, Promise<void>>()

  /** Run `task` after every earlier one for `sid` has settled. */
  serial(sid: string, task: () => Promise<void>): Promise<void> {
    const previous = this.starts.get(sid) ?? Promise.resolve()
    const next = previous.then(task, task)
    this.starts.set(sid, next)
    return next.finally(() => {
      if (this.starts.get(sid) === next) this.starts.delete(sid)
    })
  }

  /** `connection` becomes the visitor's one live connection. */
  claim(connection: Connection) {
    const previous = this.connections.get(connection.sid)
    this.connections.set(connection.sid, connection)
    this.cancelPause(connection.sid)
    if (previous && previous !== connection) previous.replace()
  }

  release(connection: Connection) {
    if (this.connections.get(connection.sid) !== connection) return
    this.connections.delete(connection.sid)
    this.schedulePause(connection.sid)
  }

  /** Live connections (for tests and /healthz). */
  get size() {
    return this.connections.size
  }

  private cancelPause(sid: string) {
    const timer = this.graceTimers.get(sid)
    if (timer) clearTimeout(timer)
    this.graceTimers.delete(sid)
  }

  private schedulePause(sid: string) {
    this.cancelPause(sid)
    const timer = setTimeout(() => {
      this.graceTimers.delete(sid)
      if (this.connections.has(sid)) return
      this.options.provider.pause(sid).catch((error) => this.log(`pause failed: ${(error as Error).message}`))
    }, this.options.pauseGraceMs)
    timer.unref?.()
    this.graceTimers.set(sid, timer)
  }

  /** "Start over": drop the live connection and delete the sandbox. */
  async reset(sid: string) {
    this.cancelPause(sid)
    const connection = this.connections.get(sid)
    this.connections.delete(sid)
    connection?.end(CLOSE.reset, "reset")
    // end() → release() found nothing to release, so no pause was scheduled.
    // Queued behind any start in flight, so that start cannot bring the
    // sandbox back after it was deleted.
    let failure: unknown
    await this.serial(sid, () => this.options.provider.reset(sid).catch((error) => void (failure = error)))
    if (failure) throw failure
  }

  /** Server shutdown. */
  close() {
    for (const timer of this.graceTimers.values()) clearTimeout(timer)
    this.graceTimers.clear()
    for (const connection of [...this.connections.values()]) connection.end(1001, "server shutting down")
  }
}

/** The admission policy: a cap on running sandboxes, then a per-IP rate. */
export function admission(options: {
  provider: SandboxProvider
  maxSandboxes: number
  limiter: { take(key: string): boolean; retryAfterMs(key: string): number }
}) {
  return async (ip: string) => {
    if ((await options.provider.count()) >= options.maxSandboxes)
      throw new LimitError("All demo sandboxes are busy right now. Please try again in a few minutes.")
    if (!options.limiter.take(ip)) {
      const minutes = Math.max(1, Math.ceil(options.limiter.retryAfterMs(ip) / 60_000))
      throw new LimitError(`Too many new sandboxes from your network. Please try again in ${minutes} min.`)
    }
  }
}
