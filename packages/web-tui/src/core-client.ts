// The page's side of the core worker: spawn it, wait for it to boot, and talk
// to it with the same JSON RPC the native TUI uses for its Bun worker
// (packages/opencode/src/util/rpc.ts, cli/cmd/tui.ts `createWorkerFetch`).
import { Rpc } from "opencode/util/rpc"
import type { rpc } from "opencode/cli/tui/worker.browser"
import type { GlobalEvent } from "@opencode-ai/sdk/v2"
import { OPENCODE_ORIGIN } from "./fetch-router"

export type CoreClient = ReturnType<typeof Rpc.client<typeof rpc>>

export type BootEvent =
  | { phase: "vfs"; storage: "opfs" | "memory"; seeded: boolean }
  | { phase: "error"; message: string }

export interface WorkerOptions {
  /** Answer http://owallet.internal with the scripted mock (tests, demos without a wallet). */
  mockOwallet?: boolean
  /** Extra environment for the core, over env.ts's ENV (e.g. OPENCODE_PRINT_LOGS). */
  env?: Record<string, string>
  /**
   * The Overpay owallet-web talks to (src/owallet.ts); defaults to norm's
   * staging Overpay. The page fills it from `?overpay=<url>`.
   */
  overpay?: { railsUrl: string; env?: string; publicUrl?: string }
}

export interface Core {
  readonly worker: Worker
  readonly client: CoreClient
  /** Resolves once the server answers RPC; rejects if the worker failed to start. */
  readonly ready: Promise<BootEvent & { phase: "vfs" }>
  /** `fetch` against the core server, e.g. `core.fetch("/session")`. */
  readonly fetch: typeof fetch
  readonly onEvent: (handler: (event: GlobalEvent) => void) => () => void
}

export function startCore(options: WorkerOptions = {}): Core {
  const worker = new Worker(new URL("./core.worker.ts", import.meta.url), {
    type: "module",
    name: JSON.stringify(options),
  })
  const client = Rpc.client<typeof rpc>(worker)
  const ready = new Promise<BootEvent & { phase: "vfs" }>((resolve, reject) => {
    const boot = { current: undefined as (BootEvent & { phase: "vfs" }) | undefined }
    client.on<BootEvent>("boot", (event) => {
      if (event.phase === "error") reject(new Error(event.message))
      if (event.phase === "vfs") boot.current = event
    })
    client.on("ready", () => resolve(boot.current ?? { phase: "vfs", storage: "memory", seeded: false }))
    worker.addEventListener("error", (event) => reject(new Error(event.message || "core worker failed to load")))
  })
  return {
    worker,
    client,
    ready,
    fetch: createWorkerFetch(client),
    onEvent: (handler) => client.on<GlobalEvent>("global.event", handler),
  }
}

// cli/cmd/tui.ts's createWorkerFetch, with relative URLs resolved against the
// core's private origin.
export function createWorkerFetch(client: CoreClient): typeof fetch {
  const fn = async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const request = new Request(
      input instanceof Request ? input : new URL(String(input), OPENCODE_ORIGIN),
      init,
    )
    const body = request.body ? await request.text() : undefined
    const result = await client.call("fetch", {
      url: request.url,
      method: request.method,
      headers: Object.fromEntries(request.headers.entries()),
      body,
    })
    return new Response(result.body, {
      status: result.status,
      headers: result.headers,
    })
  }
  return fn as typeof fetch
}
