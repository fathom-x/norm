// A `process` global for running @opentui/core in a browser. It covers what
// the renderer touches: env, platform, stdout/stderr (columns, rows, write),
// stdin (an event emitter the host feeds), signal-style events (SIGWINCH for
// resize), nextTick and hrtime. Install it before @opentui/core is imported.
import { EventEmitter } from "events"
import { Buffer } from "buffer"

export interface BrowserWriteStream extends EventEmitter {
  isTTY: boolean
  columns: number
  rows: number
  fd: number
  writable: boolean
  write(chunk: string | Uint8Array, encoding?: unknown, callback?: (error?: Error | null) => void): boolean
  getColorDepth(): number
  hasColors(): boolean
  getWindowSize(): [number, number]
  cursorTo?(): boolean
  end(): void
}

export interface BrowserReadStream extends EventEmitter {
  isTTY: boolean
  isRaw: boolean
  fd: number
  readable: boolean
  setRawMode(mode: boolean): BrowserReadStream
  setEncoding(encoding?: string): BrowserReadStream
  resume(): BrowserReadStream
  pause(): BrowserReadStream
  isPaused(): boolean
  read(): null
  ref(): BrowserReadStream
  unref(): BrowserReadStream
  /** Host side: deliver terminal input (xterm.js onData) to listeners. */
  push(data: string | Uint8Array): void
}

export interface BrowserProcess extends EventEmitter {
  env: Record<string, string | undefined>
  platform: string
  arch: string
  version: string
  versions: Record<string, string>
  argv: string[]
  execArgv: string[]
  pid: number
  exitCode: number | undefined
  browser: true
  stdout: BrowserWriteStream
  stderr: BrowserWriteStream
  stdin: BrowserReadStream
  cwd(): string
  chdir(): void
  exit(code?: number): void
  nextTick(fn: (...args: any[]) => void, ...args: any[]): void
  hrtime: ((prev?: [number, number]) => [number, number]) & { bigint(): bigint }
  uptime(): number
  memoryUsage(): { rss: number; heapTotal: number; heapUsed: number; external: number; arrayBuffers: number }
  emitWarning(warning: unknown): void
  kill(): boolean
  umask(): number
  getuid(): number
  getgid(): number
}

function createWriteStream(fd: number, sink: { write: (chunk: string | Uint8Array) => void }): BrowserWriteStream {
  const stream = new EventEmitter() as BrowserWriteStream
  stream.isTTY = true
  stream.columns = 80
  stream.rows = 24
  stream.fd = fd
  stream.writable = true
  stream.write = (chunk, encoding, callback) => {
    const cb = typeof encoding === "function" ? (encoding as (error?: Error | null) => void) : callback
    sink.write(chunk)
    if (cb) queueMicrotask(() => cb(null))
    return true
  }
  stream.getColorDepth = () => 24
  stream.hasColors = () => true
  stream.getWindowSize = () => [stream.columns, stream.rows]
  stream.end = () => {}
  return stream
}

function createReadStream(): BrowserReadStream {
  const stream = new EventEmitter() as BrowserReadStream
  let paused = true
  const queued: Buffer[] = []
  stream.isTTY = true
  stream.isRaw = false
  stream.fd = 0
  stream.readable = true
  stream.setRawMode = (mode) => {
    stream.isRaw = mode
    return stream
  }
  stream.setEncoding = () => stream
  stream.resume = () => {
    paused = false
    while (queued.length > 0 && !paused) stream.emit("data", queued.shift())
    return stream
  }
  stream.pause = () => {
    paused = true
    return stream
  }
  stream.isPaused = () => paused
  stream.read = () => null
  stream.ref = () => stream
  stream.unref = () => stream
  stream.push = (data) => {
    const chunk = typeof data === "string" ? Buffer.from(data, "utf8") : Buffer.from(data)
    if (paused || stream.listenerCount("data") === 0) {
      queued.push(chunk)
      return
    }
    stream.emit("data", chunk)
  }
  return stream
}

export interface ProcessShimOptions {
  env?: Record<string, string | undefined>
  /** Where fd 1 bytes go (JS-side writes; native frames come through WASI). */
  stdout?: (chunk: string | Uint8Array) => void
  stderr?: (chunk: string | Uint8Array) => void
  cwd?: string
}

const startTime = typeof performance !== "undefined" ? performance.now() : Date.now()

export function createBrowserProcess(options: ProcessShimOptions = {}): BrowserProcess {
  const proc = new EventEmitter() as BrowserProcess
  proc.setMaxListeners(100)
  const stdoutSink = { write: options.stdout ?? (() => {}) }
  const stderrSink = {
    write:
      options.stderr ??
      ((chunk: string | Uint8Array) => console.error(typeof chunk === "string" ? chunk : new TextDecoder().decode(chunk))),
  }
  proc.env = {
    TERM: "xterm-256color",
    COLORTERM: "truecolor",
    TERM_PROGRAM: "xterm.js",
    NODE_ENV: "production",
    ...options.env,
  }
  proc.platform = "linux"
  proc.arch = "wasm32"
  proc.version = "v22.0.0"
  proc.versions = { node: "22.0.0" }
  proc.argv = ["browser", "opentui"]
  proc.execArgv = []
  proc.pid = 1
  proc.exitCode = undefined
  proc.browser = true
  proc.stdout = createWriteStream(1, stdoutSink)
  proc.stderr = createWriteStream(2, stderrSink)
  proc.stdin = createReadStream()
  const cwd = options.cwd ?? "/"
  proc.cwd = () => cwd
  proc.chdir = () => {}
  proc.exit = (code?: number) => {
    proc.exitCode = code
    proc.emit("exit", code ?? 0)
  }
  proc.nextTick = (fn, ...args) => queueMicrotask(() => fn(...args))
  const now = () => (typeof performance !== "undefined" ? performance.now() : Date.now())
  const hrtime = ((prev?: [number, number]) => {
    const t = now()
    let sec = Math.floor(t / 1000)
    let nsec = Math.floor((t % 1000) * 1e6)
    if (prev) {
      sec -= prev[0]
      nsec -= prev[1]
      if (nsec < 0) {
        sec -= 1
        nsec += 1e9
      }
    }
    return [sec, nsec] as [number, number]
  }) as BrowserProcess["hrtime"]
  hrtime.bigint = () => BigInt(Math.round(now() * 1e6))
  proc.hrtime = hrtime
  proc.uptime = () => (now() - startTime) / 1000
  proc.memoryUsage = () => ({ rss: 0, heapTotal: 0, heapUsed: 0, external: 0, arrayBuffers: 0 })
  proc.emitWarning = (warning) => console.warn(warning)
  proc.kill = () => false
  proc.umask = () => 0o22
  proc.getuid = () => 1000
  proc.getgid = () => 1000
  return proc
}

/** Install `process` and `Buffer` globals (idempotent). */
export function installProcessShim(options: ProcessShimOptions = {}): BrowserProcess {
  const g = globalThis as any
  if (!g.Buffer) g.Buffer = Buffer
  if (!g.global) g.global = globalThis
  if (g.process && g.process.browser === true && g.process.stdin?.push) return g.process
  const proc = createBrowserProcess(options)
  g.process = proc
  return proc
}
