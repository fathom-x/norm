// The core worker's entry: Node globals, then the fetch router and the
// virtual file system, and only then the opencode core
// (packages/opencode/src/cli/tui/worker.browser.ts), so nothing in the core can
// capture `fetch` or touch the file system before they exist.
import "./shims/globals"
import type { WorkerOptions } from "./core-client"
import { installFetchRouter, OWALLET_ORIGIN } from "./fetch-router"
import { mockOwallet } from "./mock-owallet"
import { createMgmtGate, publishMgmtHeaders } from "./mgmt-gate"
import { owallet } from "./owallet"
import { serveVfs } from "./main-vfs"
import { mountVfs } from "./vfs"
import { workerRpc } from "./worker-rpc"

// The page passes its options as the worker's name (core-client.ts).
const options: WorkerOptions = (() => {
  try {
    return JSON.parse(self.name || "{}")
  } catch {
    return {}
  }
})()

Object.assign(process.env, options.env)

// owallet.internal: owallet-web (the WebAssembly owallet, src/owallet.ts —
// 503 until `bun run build:owallet` has built it), or the scripted mock when
// asked for.
// /_mgmt answers only the page and norm's host (mgmt-gate.ts).
const mgmt = createMgmtGate()
publishMgmtHeaders(mgmt)
export const router = installFetchRouter({
  [OWALLET_ORIGIN]: mgmt.wrap(options.mockOwallet ? mockOwallet : owallet(options).route),
})

const boot = (data: object) => postMessage(JSON.stringify({ type: "rpc.event", event: "boot", data }))

try {
  boot({ phase: "vfs", ...(await mountVfs()) })
  // The page mounts this tree over the channel (main-vfs.ts) for the TUI.
  if (options.vfsChannel) serveVfs(options.vfsChannel)
  const { rpc } = await import("opencode/cli/tui/worker.browser")
  // Rpc.listen looks methods up per call; add the page's private-origin fetch.
  Object.assign(rpc, workerRpc)
} catch (error) {
  console.error("[norm worker] failed to start", error)
  boot({ phase: "error", message: error instanceof Error ? `${error.message}\n${error.stack}` : String(error) })
}
