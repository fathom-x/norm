// The browser build's private origins. Code that speaks HTTP to owallet
// (`@ai-sdk/openai-compatible`, the MCP client, norm's /health and /v1/models
// probes) keeps using plain `fetch("http://owallet.internal/...")`; this
// router answers those requests in-process and lets every other URL through
// to the real `fetch` (CORS applies as usual).
//
// Install it before anything captures `fetch` — first thing in the worker
// entry (core.worker.ts), before the core is imported. On the main thread the
// same router maps the private origins onto the worker RPC.

export type Route = (request: Request) => Promise<Response>

export interface FetchRouter {
  /** Add or replace the handler for an origin, e.g. once the owallet module has loaded. */
  route(origin: string, handler: Route): void
  unroute(origin: string): void
  /** Restore the `fetch` that was there before. */
  uninstall(): void
}

export const OWALLET_ORIGIN = "http://owallet.internal"
export const OPENCODE_ORIGIN = "http://opencode.internal"

export function installFetchRouter(
  routes: Record<string, Route>,
  target: { fetch: typeof fetch; location?: { href: string } } = globalThis,
): FetchRouter {
  const table = new Map(Object.entries(routes).map(([origin, handler]) => [new URL(origin).origin, handler]))
  const original = target.fetch
  const routed = async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = new URL(input instanceof Request ? input.url : String(input), target.location?.href)
    const handler = table.get(url.origin)
    if (!handler) return original.call(target, input, init)
    return handler(input instanceof Request && !init ? input : new Request(input instanceof Request ? input : url, init))
  }
  target.fetch = Object.assign(routed, { preconnect: () => undefined }) as typeof fetch
  return {
    route: (origin, handler) => table.set(new URL(origin).origin, handler),
    unroute: (origin) => table.delete(new URL(origin).origin),
    uninstall: () => {
      target.fetch = original
    },
  }
}

/** Until owallet-web (the WebAssembly build of owallet) is plugged in. */
export const owalletUnavailable: Route = async (request) =>
  Response.json(
    {
      error: {
        code: "owallet_unavailable",
        message: `owallet is not loaded in this build yet (${request.method} ${new URL(request.url).pathname})`,
      },
    },
    { status: 503 },
  )
