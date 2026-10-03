// `process` for the browser build: the slice of Node's process object that
// opencode's core and its dependencies read. The page decides `env` (see
// `ENV` in ../env.ts); everything that would touch a real OS is inert.
import { EventEmitter } from "events"

const started = performance.now()
const emitter = new EventEmitter()
const state = { cwd: "/workspace", exitCode: undefined as number | undefined }

function hrtime(previous?: [number, number]): [number, number] {
  const now = performance.now() - started
  const seconds = Math.floor(now / 1000)
  const nanos = Math.floor((now % 1000) * 1e6)
  if (!previous) return [seconds, nanos]
  const diff = seconds * 1e9 + nanos - (previous[0] * 1e9 + previous[1])
  return [Math.floor(diff / 1e9), diff % 1e9]
}
hrtime.bigint = () => BigInt(Math.floor((performance.now() - started) * 1e6))

const stream = (fd: number) => ({
  fd,
  isTTY: false,
  columns: 80,
  rows: 24,
  write(chunk: unknown) {
    const text = typeof chunk === "string" ? chunk : new TextDecoder().decode(chunk as Uint8Array)
    if (text.trim()) (fd === 2 ? console.warn : console.log)(text.replace(/\n$/, ""))
    return true
  },
  on: () => undefined,
  once: () => undefined,
  off: () => undefined,
  removeListener: () => undefined,
  end: () => undefined,
})

const own = Object.assign(emitter, {
  title: "browser",
  browser: true,
  platform: "linux" as const,
  arch: "wasm32",
  version: "v22.0.0",
  versions: { node: "22.0.0" } as Record<string, string>,
  release: { name: "node" },
  pid: 1,
  ppid: 0,
  argv: ["/usr/bin/node", "/norm/bin/norm"],
  argv0: "node",
  execArgv: [] as string[],
  execPath: "/usr/bin/node",
  env: {} as Record<string, string | undefined>,
  stdin: stream(0),
  stdout: stream(1),
  stderr: stream(2),
  get exitCode() {
    return state.exitCode
  },
  set exitCode(code: number | undefined) {
    state.exitCode = code
  },
  cwd: () => state.cwd,
  chdir: (directory: string) => {
    state.cwd = directory
  },
  umask: () => 0o022,
  getuid: () => 1000,
  getgid: () => 1000,
  geteuid: () => 1000,
  getegid: () => 1000,
  uptime: () => (performance.now() - started) / 1000,
  hrtime,
  memoryUsage: Object.assign(() => ({ rss: 0, heapTotal: 0, heapUsed: 0, external: 0, arrayBuffers: 0 }), {
    rss: () => 0,
  }),
  cpuUsage: () => ({ user: 0, system: 0 }),
  resourceUsage: () => ({}),
  nextTick: (fn: (...args: unknown[]) => void, ...args: unknown[]) => queueMicrotask(() => fn(...args)),
  emitWarning: (warning: unknown) => console.warn(warning),
  exit: (code?: number) => {
    throw new Error(`process.exit(${code ?? ""}) is not available in the browser build`)
  },
  kill: () => false,
  abort: () => {
    throw new Error("process.abort() is not available in the browser build")
  },
  binding: () => {
    throw new Error("process.binding() is not available in the browser build")
  },
  features: {},
  config: { variables: {} },
})

/**
 * The process object every `process` import sees. In the worker that is this
 * module's (installed as the global by globals.ts). On the page opentui-wasm
 * installs its own first (bootOpenTUIWasm: stdin/stdout wired to xterm.js)
 * and `completeProcess` fills in the rest, so both views agree.
 */
export const process: typeof own = existing() ?? own

function existing() {
  const current = (globalThis as { process?: { browser?: boolean } }).process
  return current?.browser === true ? (current as typeof own) : undefined
}

/** Give opentui-wasm's process the members only this shim has (cwd, chdir, env, ...). */
export function completeProcess(target: Record<string, unknown>, env: Record<string, string>) {
  const source = own as unknown as Record<string, unknown>
  for (const key of Object.keys(source))
    if (!(key in target) && !key.startsWith("_")) target[key] = source[key]
  Object.assign(target.env as Record<string, string>, env)
  target.cwd = own.cwd
  target.chdir = own.chdir
  target.argv = own.argv
  target.hrtime = own.hrtime
  return target
}

export default process
