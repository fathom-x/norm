// Demo host: xterm.js + @opentui/core on the wasm core.
// ?scene=core (default) renders a Box/Text/Input scene with the imperative
// API; ?scene=solid renders a small @opentui/solid component.
import { Terminal } from "@xterm/xterm"
import "@xterm/xterm/css/xterm.css"
import wasmUrl from "../dist/opentui.wasm?url"
import { bootOpenTUIWasm } from "../src/boot"

const status = document.getElementById("status")!
const params = new URLSearchParams(location.search)
const cols = Number(params.get("cols") ?? 80)
const rows = Number(params.get("rows") ?? 24)

const term = new Terminal({
  cols,
  rows,
  allowProposedApi: true,
  fontFamily: "ui-monospace, Menlo, Consolas, monospace",
  fontSize: 14,
  theme: { background: "#101014" },
})
term.open(document.getElementById("terminal")!)
term.focus()
;(window as any).term = term

const t0 = performance.now()
const host = await bootOpenTUIWasm({ wasm: fetch(wasmUrl), terminal: term })
;(window as any).opentuiHost = host
const bootMs = performance.now() - t0

const scene = params.get("scene") ?? "core"
const mod = scene === "solid" ? await import("./solid-scene") : await import("./core-scene")
const info = await mod.start()
;(window as any).demo = info
status.textContent = `scene=${scene} · wasm boot ${bootMs.toFixed(0)} ms · ready`
;(window as any).demoReady = true
