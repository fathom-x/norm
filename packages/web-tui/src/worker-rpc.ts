// RPC methods the core worker adds to worker.browser.ts's `rpc` (which
// Rpc.listen looks up per call, so extending the object is enough — the
// opencode-side twin of worker.ts stays the same shape as the native one).
import { OWALLET_ORIGIN } from "./fetch-router"

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
    const response = await fetch(input.url, {
      method: input.method,
      headers: input.headers,
      body: input.body || undefined,
    })
    return {
      status: response.status,
      headers: Object.fromEntries(response.headers.entries()),
      body: await response.text(),
    }
  },
}

export type WorkerRpc = typeof workerRpc
