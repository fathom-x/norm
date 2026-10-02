/**
 * How norm reaches owallet. Natively that is a process — `owallet serve`
 * listening on localhost, plus one-shot CLI calls (`provider-key create`) —
 * and `Norm`'s own code drives it. In the browser build there are no
 * processes or ports: owallet is a WebAssembly module answering `fetch`
 * requests for `http://owallet.internal` (see packages/web-tui), and the CLI
 * verbs are JSON routes under `/_mgmt`. Everything that speaks HTTP to owallet
 * (`/v1`, `/mcp`, `/health`, `/v1/status`) is the same in both; only bringing
 * it up and minting norm's key differ, which is what this interface covers.
 */
export interface OwalletHost {
  readonly kind: "process" | "wasm"
  /** Bring owallet up at `base` (or confirm it is); whether it answers afterwards. */
  ensureServer(base: string): Promise<boolean>
  /** Why norm's key can't be minted right now; undefined when it can. */
  mintBlocker(): Promise<string | undefined>
  /** Mint a provider key; undefined on failure (the reason goes to NORM_DEBUG). */
  mintProviderKey(input: MintInput): Promise<string | undefined>
}

export type MintInput = { label: string; spend: boolean; budgetUsd: number }

/** The private origin the browser build's fetch router hands to owallet-web. */
export const BROWSER_OWALLET_URL = "http://owallet.internal"

/**
 * True in the browser build. An explicit flag rather than sniffing for
 * `window`/`WorkerGlobalScope`: the native TUI also runs norm's core inside a
 * (Bun) Worker, and must keep the process host. The page seeds it in the
 * `process.env` polyfill it gives both the worker and the main thread.
 */
export function isBrowser(): boolean {
  return typeof process !== "undefined" && process.env?.NORM_RUNTIME === "browser"
}

/** What `GET /_mgmt/status` reports (owallet-web). */
export type WasmStatus = {
  version?: string
  initialized?: boolean
  unlocked?: boolean
  wallet?: { npub?: string } | null
  overpay_linked?: boolean
}

/**
 * owallet compiled to WebAssembly (owallet/crates/owallet-web), reached
 * through the page's fetch router. "Bringing it up" is the setup screen's job
 * (create/unlock the wallet before the TUI starts), so `ensureServer` only
 * checks that the module answers and the wallet is unlocked.
 */
export function wasmHost(debug: (...args: unknown[]) => void, base = BROWSER_OWALLET_URL): OwalletHost {
  const status = async (): Promise<WasmStatus | undefined> => {
    try {
      const res = await fetch(`${base}/_mgmt/status`, { signal: AbortSignal.timeout(5000) })
      if (!res.ok) {
        debug(`owallet-web status responded ${res.status}`)
        return undefined
      }
      return (await res.json()) as WasmStatus
    } catch (error) {
      debug("owallet-web is not reachable:", error)
      return undefined
    }
  }
  return {
    kind: "wasm",
    async ensureServer() {
      const current = await status()
      if (!current) return false
      if (!current.unlocked) debug("owallet-web answers but the wallet is locked — unlock it on the setup screen")
      return true
    },
    async mintBlocker() {
      const current = await status()
      if (!current) return "owallet-web is not reachable"
      if (!current.initialized) return "no wallet database yet — create one on the setup screen"
      if (!current.unlocked) return "the wallet is locked — unlock it on the setup screen"
      if (!current.wallet?.npub) return "no wallet selected — generate or import one on the setup screen"
      return undefined
    },
    async mintProviderKey(input) {
      try {
        const res = await fetch(`${base}/_mgmt/provider-key/create`, {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ label: input.label, spend: input.spend, budget_usd: input.budgetUsd }),
          signal: AbortSignal.timeout(30_000),
        })
        const body: any = await res.json().catch(() => undefined)
        if (!res.ok) {
          debug(`provider-key create failed (${res.status}):`, body?.error?.message ?? body)
          return undefined
        }
        const key = body?.key
        if (typeof key !== "string" || !key.startsWith("owk_")) {
          debug("provider-key create returned an unexpected payload")
          return undefined
        }
        return key
      } catch (error) {
        debug("provider-key create failed:", error)
        return undefined
      }
    },
  }
}

export * as NormHost from "./host"
