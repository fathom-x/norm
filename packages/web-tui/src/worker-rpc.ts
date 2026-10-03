// RPC methods the core worker adds to worker.browser.ts's `rpc` (which
// Rpc.listen looks up per call, so extending the object is enough — the
// opencode-side twin of worker.ts stays the same shape as the native one).
import { OWALLET_ORIGIN } from "./fetch-router"
import { MGMT_GLOBAL } from "./mgmt-gate"

export type SerializedRequest = { url: string; method: string; headers: Record<string, string>; body?: string }
export type SerializedResponse = { status: number; headers: Record<string, string>; body: string }

/** Origins the page may reach through the worker's fetch router. */
const PRIVATE_ORIGINS = new Set([OWALLET_ORIGIN])

export const workerRpc = {
  /**
   * The page's requests for http://owallet.internal (the TUI's owallet
   * sidebar reads /v1/status and /v1/models): answered by whatever the
   * worker's router has registered there — owallet-web, the scripted mock, or
   * the 503 stub. Bodies are buffered.
   */
  async privateFetch(input: SerializedRequest): Promise<SerializedResponse> {
    const origin = new URL(input.url).origin
    if (!PRIVATE_ORIGINS.has(origin))
      return { status: 403, headers: { "content-type": "text/plain" }, body: `${origin} is not a private origin` }
    try {
      const response = await fetch(input.url, {
        method: input.method,
        // The page is trusted with owallet's /_mgmt (mgmt-gate.ts).
        headers: { ...input.headers, ...((globalThis as Record<string, unknown>)[MGMT_GLOBAL] as object) },
        body: input.body || undefined,
      })
      return {
        status: response.status,
        headers: Object.fromEntries(response.headers.entries()),
        body: await response.text(),
      }
    } catch (error) {
      // Rpc.listen never answers a method that throws, which would leave the
      // page waiting forever; answer with the failure instead.
      return {
        status: 502,
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          error: { code: "private_fetch_failed", message: error instanceof Error ? error.message : String(error) },
        }),
      }
    }
  },
}

export type WorkerRpc = typeof workerRpc
