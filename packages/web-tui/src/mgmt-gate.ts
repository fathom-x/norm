// owallet-web's /_mgmt routes (create/unlock the wallet, mint provider keys,
// link Overpay) stand in for CLI verbs that natively only the user can run.
// In the core worker every HTTP client reaches http://owallet.internal through
// the fetch router — the model's webfetch, a provider or MCP URL the model
// wrote into a config file — so /_mgmt requires a per-boot capability header
// that only the two trusted callers present: the page (setup screen, through
// worker-rpc.ts privateFetch) and norm's wasm host
// (packages/opencode/src/norm/host.ts, which reads it from the global below;
// the model has no way to run code in the worker).
import type { Route } from "./fetch-router"

export const MGMT_HEADER = "x-norm-mgmt"

/** Where norm's host finds the header (host.ts `mgmtHeaders`). */
export const MGMT_GLOBAL = "__normOwalletMgmt"

export interface MgmtGate {
  readonly headers: Record<string, string>
  /** `route` with /_mgmt refused unless the request carries the capability. */
  wrap(route: Route): Route
}

export function createMgmtGate(token: string = crypto.randomUUID()): MgmtGate {
  const headers = { [MGMT_HEADER]: token }
  return {
    headers,
    wrap: (route) => async (request) => {
      const { pathname } = new URL(request.url)
      if (pathname.startsWith("/_mgmt") && request.headers.get(MGMT_HEADER) !== token)
        return Response.json(
          { error: { code: "forbidden", message: "owallet's management API is only for norm itself" } },
          { status: 403 },
        )
      return route(request)
    },
  }
}

/** Publish the gate's header for norm's wasm host in this worker. */
export function publishMgmtHeaders(gate: MgmtGate, target: Record<string, unknown> = globalThis as never) {
  target[MGMT_GLOBAL] = { ...gate.headers }
}
