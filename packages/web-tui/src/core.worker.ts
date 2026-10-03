// The core worker's entry: Node globals, then the fetch router and the
// virtual file system, and only then the opencode core
// (packages/opencode/src/cli/tui/worker.browser.ts), so nothing in the core can
// capture `fetch` or touch the file system before they exist.
import "./shims/globals"
import { installFetchRouter, OWALLET_ORIGIN, owalletUnavailable } from "./fetch-router"
import { mountVfs } from "./vfs"

export const router = installFetchRouter({ [OWALLET_ORIGIN]: owalletUnavailable })

try {
  const vfs = await mountVfs()
  postMessage(JSON.stringify({ type: "rpc.event", event: "boot", data: { phase: "vfs", ...vfs } }))
  await import("opencode/cli/tui/worker.browser")
} catch (error) {
  console.error("[norm worker] failed to start", error)
  postMessage(
    JSON.stringify({
      type: "rpc.event",
      event: "boot",
      data: { phase: "error", message: error instanceof Error ? `${error.message}\n${error.stack}` : String(error) },
    }),
  )
}
