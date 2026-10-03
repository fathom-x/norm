// The norm TUI on the page: xterm.js + opentui on its wasm core
// (packages/opentui-wasm), talking to the core worker exactly like the native
// TUI talks to its Bun worker (packages/opencode/src/cli/cmd/tui.ts).
//
// Order matters, and is why the TUI is a dynamic import at the end:
// 1. the terminal, sized to its container;
// 2. bootOpenTUIWasm installs opentui-wasm's `process` (stdin/stdout wired to
//    xterm) and instantiates the core; completeProcess adds what the norm
//    code expects (env, cwd = /workspace, Node-style timers) and `Bun`;
// 3. the worker's file tree is mounted (main-vfs.ts) and owallet.internal is
//    routed to the worker, before any TUI module reads a file or the network;
// 4. only then opentui, the TUI and the opencode modules it needs load.
import { FitAddon } from "@xterm/addon-fit"
import { Terminal } from "@xterm/xterm"
import "@xterm/xterm/css/xterm.css"
import wasmUrl from "../../opentui-wasm/dist/opentui.wasm?url"
import { bootOpenTUIWasm } from "../../opentui-wasm/src/boot"
import type { Core } from "./core-client"
import { ENV } from "./env"
import { installFetchRouter, OWALLET_ORIGIN } from "./fetch-router"
import { mountWorkerVfs } from "./main-vfs"
import { installBunGlobal } from "./shims/bun"
import { completeProcess } from "./shims/process"
import { installNodeTimers } from "./shims/timers"

export interface TuiOptions {
  core: Core
  container: HTMLElement
  vfsChannel: string
  /** `?session=` — open this session. */
  sessionID?: string
  /** `?prompt=` — prefill (and submit) this prompt. */
  prompt?: string
  /** The wasm core, fetched early by the caller (it is ~2 MB). */
  wasm?: Promise<Response>
}

export async function startTui(options: TuiOptions) {
  const term = new Terminal({
    allowProposedApi: true,
    cursorBlink: false,
    fontFamily: '"JetBrains Mono", ui-monospace, SFMono-Regular, Menlo, Consolas, monospace',
    fontSize: 14,
    theme: { background: "#0a0a0a" },
  })
  const fit = new FitAddon()
  term.loadAddon(fit)
  term.open(options.container)
  fit.fit()
  term.focus()
  Object.assign(globalThis, { __norm: { term, core: options.core } })

  const placeholder = (globalThis as { process?: { env?: Record<string, string> } }).process
  const host = await bootOpenTUIWasm({ wasm: options.wasm ?? fetch(wasmUrl), terminal: term, env: ENV })
  // Keep the env object modules may already hold (main.ts's placeholder).
  if (placeholder?.env) host.process.env = Object.assign(placeholder.env, host.process.env)
  completeProcess(host.process as unknown as Record<string, unknown>, ENV)
  installBunGlobal()
  installNodeTimers()

  // Keep the grid matched to the viewport; boot already forwards term resizes.
  const refit = () => fit.fit()
  new ResizeObserver(refit).observe(options.container)
  window.addEventListener("resize", refit)
  options.container.addEventListener("pointerdown", () => term.focus())

  await mountWorkerVfs(options.vfsChannel)
  installFetchRouter({ [OWALLET_ORIGIN]: options.core.privateFetch })

  const { runTui } = await import("./tui-run")
  Object.assign(globalThis, { __norm: { term, core: options.core, host } })
  await runTui(options)
  return { term, host }
}
