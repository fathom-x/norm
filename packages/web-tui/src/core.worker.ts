// The core worker's entry: Node globals, then the fetch router and the
// virtual file system, and only then the opencode core
// (packages/opencode/src/cli/tui/worker.browser.ts), so nothing in the core can
// capture `fetch` or touch the file system before they exist.
import "./shims/globals"
import type { WorkerOptions } from "./core-client"
import { installFetchRouter, OWALLET_ORIGIN } from "./fetch-router"
import { mockOwallet } from "./mock-owallet"
import { owallet } from "./owallet"
import { mountVfs } from "./vfs"

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
export const router = installFetchRouter({
  [OWALLET_ORIGIN]: options.mockOwallet ? mockOwallet : owallet(options).route,
})

const boot = (data: object) => postMessage(JSON.stringify({ type: "rpc.event", event: "boot", data }))

try {
  boot({ phase: "vfs", ...(await mountVfs()) })
  await import("opencode/cli/tui/worker.browser")
} catch (error) {
  console.error("[norm worker] failed to start", error)
  boot({ phase: "error", message: error instanceof Error ? `${error.message}\n${error.stack}` : String(error) })
}
