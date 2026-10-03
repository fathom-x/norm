// owallet-web — owallet compiled to WebAssembly (owallet/crates/owallet-web) —
// answering http://owallet.internal inside the core worker. `/v1`, `/mcp`,
// `/health` and the `/_mgmt` setup API all go through its exported
// `handle(Request) → Promise<Response>`; response bodies stream, so SSE
// reaches the AI SDK chunk by chunk.
//
// Loaded lazily: the core boots without waiting for ~7 MB of wasm, and the
// first owallet request awaits the module (and the wallet database's VFS).
// A missing build or a failed start answers 503 JSON naming the cause, never
// a hang. Build the module first with `bun run build:owallet`
// (scripts/build-owallet-web.sh); without it the page still builds and
// owallet.internal reports itself unavailable.
//
// The page reaches it through the worker's `privateFetch` RPC (worker-rpc.ts),
// which goes through the same router. To point owallet at another Overpay at
// runtime (the setup screen, tests), the page posts
// `{type: "owallet.configure", id, overpay}` to the worker
// (`configureOwallet(worker, overpay)`); the wallet database is kept and the
// router rebuilt for the next request.
import { abortable, followAbort } from "./abort"
import type { WorkerOptions } from "./core-client"
import type { Route } from "./fetch-router"

/** Which Overpay the wallet talks to. */
export interface OverpayTarget {
  /** Overpay API base URL (bearers are filed under it). */
  railsUrl: string
  /** "prod" | "dev" | "staging"; what /_mgmt/status reports. */
  env?: string
  /** Browser-facing Overpay URL, if different from railsUrl. */
  publicUrl?: string
}

/** norm's pre-release default (DEFAULT_ENV in packages/opencode/src/norm/norm.ts). */
export const DEFAULT_OVERPAY: OverpayTarget = { railsUrl: "https://overpay-eykm.onrender.com", env: "staging" }

/** Wallet database file and the OPFS directory of its access-handle pool. */
export const OWALLET_DB = "owallet.db"
export const OWALLET_OPFS_DIRECTORY = ".owallet-web"
const START_TIMEOUT_MS = 30_000

interface OwalletWeb {
  default(input?: { module_or_path: string }): Promise<unknown>
  init(config: Record<string, string>): Promise<void>
  handle(request: Request): Promise<Response>
}

// Globs, not imports: an unbuilt module leaves these empty instead of
// failing the page build.
const modules = import.meta.glob<OwalletWeb>("./owallet-web/owallet_web.js")
const wasmUrls = import.meta.glob<string>("./owallet-web/owallet_web_bg.wasm", { query: "?url", import: "default" })

/** OPFS sync access handles exist only in dedicated workers (and not in every browser). */
export function opfsAvailable(scope: any = globalThis): boolean {
  return (
    typeof scope.FileSystemSyncAccessHandle === "function" &&
    typeof scope.navigator?.storage?.getDirectory === "function" &&
    typeof scope.WorkerGlobalScope === "function"
  )
}

/** The `init(config)` object owallet-web takes. */
export function owalletConfig(target: OverpayTarget, storage: "opfs" | "memory"): Record<string, string> {
  return {
    rails_url: target.railsUrl,
    ...(target.publicUrl && { public_url: target.publicUrl }),
    env: target.env ?? "prod",
    storage,
    db_name: OWALLET_DB,
    opfs_directory: OWALLET_OPFS_DIRECTORY,
  }
}

export interface Owallet {
  readonly route: Route
  /** Re-point owallet at another Overpay (takes effect for the next request). */
  configure(target: OverpayTarget): void
}

/** The real owallet for the worker's fetch router (core.worker.ts), plus the page bridge. */
export function owallet(options: WorkerOptions, scope: any = globalThis): Owallet {
  let target: OverpayTarget = options.overpay?.railsUrl ? options.overpay : DEFAULT_OVERPAY
  let started: Promise<OwalletWeb> | undefined
  let module: Promise<OwalletWeb> | undefined

  const load = () =>
    (module ??= (async () => {
      const js = modules["./owallet-web/owallet_web.js"]
      const wasm = wasmUrls["./owallet-web/owallet_web_bg.wasm"]
      if (!js || !wasm) throw new Unavailable("owallet-web is not built into this page — run `bun run build:owallet`, then rebuild")
      const mod = await js()
      await mod.default({ module_or_path: await wasm() })
      return mod
    })())

  const start = () =>
    (started ??= withTimeout(
      (async () => {
        const mod = await load()
        const storage = opfsAvailable(scope) ? "opfs" : "memory"
        await mod.init(owalletConfig(target, storage))
        return mod
      })(),
      START_TIMEOUT_MS,
      "owallet-web did not start in time — is norm open in another tab? (the wallet database can only be open in one)",
    ).catch((error) => {
      // A failed start is retried on the next request (a configure() may fix it).
      started = undefined
      throw error
    }))

  const route: Route = async (request) => {
    let mod: OwalletWeb
    try {
      mod = await start()
    } catch (error) {
      return unavailable(error)
    }
    let response: Response
    try {
      response = await abortable(mod.handle(request), request.signal)
    } catch (error) {
      if (request.signal.aborted) throw request.signal.reason ?? error
      return unavailable(error, "owallet_error")
    }
    return followAbort(response, request.signal)
  }

  const configure = (next: OverpayTarget) => {
    target = next
    started = undefined
  }

  bridge(scope, configure)
  return { route, configure }
}

class Unavailable extends Error {}

function unavailable(error: unknown, code = "owallet_unavailable"): Response {
  const message = error instanceof Error ? error.message : String(error)
  console.warn("[norm worker] owallet-web:", message)
  return Response.json({ error: { code, message } }, { status: 503 })
}

function withTimeout<T>(promise: Promise<T>, ms: number, message: string): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined
  return Promise.race([
    promise,
    new Promise<never>((_, reject) => {
      timer = setTimeout(() => reject(new Unavailable(message)), ms)
    }),
  ]).finally(() => clearTimeout(timer))
}

// ---- page bridge: owallet.configure ----------------------------------------

type ConfigureMessage = { type: "owallet.configure"; id: string; overpay: OverpayTarget }

/** Worker side (ignored by the core's RPC, which only reads `rpc.*` messages). */
function bridge(scope: any, configure: (target: OverpayTarget) => void) {
  if (typeof scope.addEventListener !== "function" || typeof scope.postMessage !== "function") return
  scope.addEventListener("message", (event: MessageEvent) => {
    if (typeof event.data !== "string" || !event.data.includes('"owallet.configure"')) return
    const message = JSON.parse(event.data) as ConfigureMessage
    if (message.type !== "owallet.configure" || !message.overpay?.railsUrl) return
    configure(message.overpay)
    scope.postMessage(JSON.stringify({ type: "owallet.configured", id: message.id }))
  })
}

/** Page side: point the worker's owallet at another Overpay; resolves once it took. */
export function configureOwallet(worker: Worker, overpay: OverpayTarget): Promise<void> {
  const id = `owallet-${Math.random().toString(36).slice(2)}`
  return new Promise((resolve) => {
    const onMessage = (event: MessageEvent) => {
      if (typeof event.data !== "string" || !event.data.includes(id)) return
      worker.removeEventListener("message", onMessage)
      resolve()
    }
    worker.addEventListener("message", onMessage)
    worker.postMessage(JSON.stringify({ type: "owallet.configure", id, overpay }))
  })
}
