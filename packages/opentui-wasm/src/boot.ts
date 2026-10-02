// Boot @opentui/core on its wasm core in a browser and connect it to a
// terminal emulator (xterm.js or anything with the same small surface).
//
//   const host = await bootOpenTUIWasm({ wasm: fetch(wasmUrl), terminal })
//   const { createCliRenderer } = await import("@opentui/core")
//
// @opentui/core must be imported *after* this resolves (dynamic import): its
// module initialisation reads `process` and picks the FFI backend from
// globalThis.__OPENTUI_WASM__.
import { installProcessShim, type BrowserProcess } from "./shims/process.js"
import { instantiateOpenTUIWasm, type OpenTUIWasmInstance } from "@opentui/core/wasm"

export interface TerminalLike {
  readonly cols: number
  readonly rows: number
  write(data: string | Uint8Array): void
  onData(listener: (data: string) => void): unknown
  onBinary?(listener: (data: string) => void): unknown
  onResize(listener: (size: { cols: number; rows: number }) => void): unknown
}

export interface BootOptions {
  /** The core module: a fetch() promise/Response, bytes, or a compiled module. */
  wasm: Response | Promise<Response> | BufferSource | WebAssembly.Module
  terminal: TerminalLike
  /** Extra environment for both the JS side (process.env) and the core (WASI environ). */
  env?: Record<string, string>
}

export interface OpenTUIWasmHost {
  wasm: OpenTUIWasmInstance
  process: BrowserProcess
  /** Feed raw terminal input (already wired to terminal.onData). */
  input(data: string | Uint8Array): void
  /** Propagate a size change (already wired to terminal.onResize). */
  resize(cols: number, rows: number): void
}

export async function bootOpenTUIWasm(options: BootOptions): Promise<OpenTUIWasmHost> {
  const { terminal } = options
  const proc = installProcessShim({
    env: options.env,
    stdout: (chunk) => terminal.write(chunk),
  })
  proc.stdout.columns = terminal.cols
  proc.stdout.rows = terminal.rows

  const wasm = await instantiateOpenTUIWasm(options.wasm, {
    // Rendered frames: the core's StdoutOutput writes to fd 1.
    stdout: (bytes) => terminal.write(bytes),
    stderr: (bytes) => console.error(new TextDecoder().decode(bytes)),
    env: { ...(proc.env as Record<string, string>) },
  })
  globalThis.__OPENTUI_WASM__ = wasm

  const input = (data: string | Uint8Array) => proc.stdin.push(data)
  const resize = (cols: number, rows: number) => {
    if (proc.stdout.columns === cols && proc.stdout.rows === rows) return
    proc.stdout.columns = cols
    proc.stdout.rows = rows
    proc.stdout.emit("resize")
    // CliRenderer listens for SIGWINCH on process when it owns process.stdout.
    proc.emit("SIGWINCH")
  }
  terminal.onData(input)
  terminal.onBinary?.((data) => input(Uint8Array.from(data, (c) => c.charCodeAt(0) & 0xff)))
  terminal.onResize(({ cols, rows }) => resize(cols, rows))

  return { wasm, process: proc, input, resize }
}
