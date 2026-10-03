// norm in the browser. The core (opencode server) runs in a Web Worker
// (core.worker.ts); the page hosts the real norm TUI in xterm.js (tui.ts).
//
// URL flags: ?mock-owallet (scripted owallet, see mock-owallet.ts), ?debug
// (core logs + norm diagnostics in the console), ?debug-panel (the plain-DOM
// debug panel instead of the TUI), ?overpay=<url> (the Overpay owallet-web
// talks to), ?session=<id>, ?prompt=<text>.
import wasmUrl from "../../opentui-wasm/dist/opentui.wasm?url"
import { startCore } from "./core-client"
import { ENV } from "./env"
import { OWALLET_ORIGIN } from "./fetch-router"

// The build points `process.env` at `globalThis.process.env`, and some
// modules read it as they load. Until opentui-wasm installs the page's real
// process (tui.ts), this placeholder carries the env; tui.ts keeps the same
// env object.
;(globalThis as { process?: unknown }).process ??= { env: { ...ENV } }

const params = new URLSearchParams(location.search)
const root = document.querySelector<HTMLElement>("#app")!
// The worker serves its file tree to the page on this channel (main-vfs.ts);
// per page, so two tabs never answer each other.
const vfsChannel = `norm-vfs-${crypto.randomUUID()}`
const core = startCore({
  mockOwallet: params.has("mock-owallet"),
  env: params.has("debug") ? { OPENCODE_PRINT_LOGS: "1", NORM_DEBUG: "1" } : {},
  overpay: params.get("overpay") ? { railsUrl: params.get("overpay")! } : undefined,
  vfsChannel,
})

if (params.has("debug-panel")) {
  const { startDebugPanel } = await import("./debug-panel")
  startDebugPanel({ core, root })
} else {
  // Fetch the opentui wasm core while the worker boots.
  const wasm = fetch(wasmUrl)
  root.classList.add("tui")
  root.dataset.state = "booting"
  try {
    await core.ready
    // The wallet before the TUI: create or unlock it, link Overpay (the
    // browser twin of norm's TTY first-run prompts). The scripted mock has no
    // wallet to set up.
    if (!params.has("mock-owallet")) await setUpWallet()
    const { startTui } = await import("./tui")
    root.dataset.state = "ready"
    await startTui({
      core,
      container: root,
      vfsChannel,
      wasm,
      sessionID: params.get("session") ?? undefined,
      prompt: params.get("prompt") ?? undefined,
    })
    root.dataset.state = "exited"
  } catch (error) {
    root.dataset.state = "error"
    root.textContent = `norm failed to start: ${error instanceof Error ? error.message : String(error)}`
    console.error(error)
  }
}

async function setUpWallet() {
  const [{ runSetup }, { resetBrowserState }] = await Promise.all([
    import("./setup/setup"),
    import("./reset"),
    import("./setup/setup.css"),
  ])
  const screen = document.createElement("main")
  document.body.prepend(screen)
  root.hidden = true
  try {
    await runSetup(screen, {
      owallet: (path, init) => core.privateFetch(`${OWALLET_ORIGIN}${path}`, init),
      origin: location.origin,
      reset: () => resetBrowserState({ stopCore: () => core.worker.terminate() }),
    })
  } finally {
    screen.remove()
    root.hidden = false
  }
  // norm's bootstrap ran when the core started, before the wallet was
  // unlocked; run it again so it mints (or re-checks) norm's provider key.
  await core.client.call("reload", undefined)
}
