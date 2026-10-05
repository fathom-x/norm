import { Flock } from "@opencode-ai/core/util/flock"
import { Norm } from "./norm"

// `owallet serve` is one detached process shared by every norm window on a
// wallet, and norm only starts it at launch. When it dies under a running
// norm (killed, crashed, replaced by an upgrade), every request used to fail
// with the AI SDK's "Cannot connect to API" after five blind retries against
// a server that was never coming back. Requests to owallet go through `fetch`
// below instead: a request that cannot connect while owallet is down restarts
// it, the way launch would have, and is sent again. Only when that is not
// possible does the user see an error, and then one that says what to do.

/** owallet is down and norm could not bring it back. */
export class OwalletDownError extends Error {
  constructor(
    readonly reason: Exclude<Norm.ServerDown, "remote">,
    options?: ErrorOptions,
  ) {
    super(message(reason), options)
    this.name = "OwalletDownError"
  }
}

// Worded to stay clear of the session retry patterns (session/retry.ts):
// retrying cannot fix any of these.
export function message(reason: Exclude<Norm.ServerDown, "remote">) {
  if (reason === "no-password")
    return "owallet has stopped, and norm cannot restart it without the wallet password. Quit norm and open it again to enter the password, or start `owallet serve` yourself."
  if (reason === "no-binary")
    return "owallet has stopped, and norm cannot find the owallet program to restart it. Run `norm upgrade` to install it again."
  if (reason === "no-wallet")
    return "owallet has stopped, and its wallet database is missing, so norm cannot restart it. Run `norm debug norm` to see where norm expects the wallet."
  return "owallet has stopped and did not come back when norm restarted it. Run `norm debug norm` to check its setup."
}

export interface Deps {
  /** The owallet address requests are expected to go to. */
  readonly base: () => string
  readonly reachable: (base: string) => Promise<boolean>
  readonly start: (base: string) => Promise<Norm.ServerStart>
  readonly fetch: typeof globalThis.fetch
  /** owallet was down and is answering again. */
  readonly revived: () => void
}

/** A `fetch` that brings owallet back when a request finds it down. */
export function make(deps: Deps) {
  // One restart at a time: a chat request and its title request fail together.
  let starting: Promise<Norm.ServerStart> | undefined
  // Counts restarts, so a request that failed before one finished can tell
  // that owallet has been brought back since it was sent.
  let revivals = 0
  const start = (base: string) =>
    (starting ??= deps
      .start(base)
      .then((result) => {
        if (!result.ok) return result
        revivals++
        deps.revived()
        return result
      })
      .finally(() => {
        starting = undefined
      }))

  return (input: RequestInfo | URL, init?: RequestInit) => {
    const sent = revivals
    return deps.fetch(input, init).catch(async (error) => {
      const base = deps.base()
      // Not ours to fix: the caller gave up, the request went somewhere
      // else, or owallet is up and this was an ordinary network error.
      if (init?.signal?.aborted || !targets(input, base)) throw error
      if (sent === revivals) {
        if (await deps.reachable(base)) throw error
        const result = sent === revivals ? await start(base) : { ok: true as const }
        if (!result.ok && result.reason !== "remote") throw new OwalletDownError(result.reason, { cause: error })
        if (!result.ok) throw error
      }
      // A body that was a stream is spent. The original error is retryable,
      // so the session's own retry sends the request again.
      if (init?.signal?.aborted || !replayable(input, init)) throw error
      return deps.fetch(input, init)
    })
  }
}

function targets(input: RequestInfo | URL, base: string) {
  const url = typeof input === "string" ? input : input instanceof URL ? input.href : input.url
  return URL.canParse(url) && URL.canParse(base) && new URL(url).origin === new URL(base).origin
}

function replayable(input: RequestInfo | URL, init?: RequestInit) {
  return !(input instanceof Request) && !(init?.body instanceof ReadableStream)
}

const listeners = new Map<string, () => void>()

/**
 * Run `listener` whenever owallet was brought back. One listener per key, so
 * an instance that reloads replaces its own instead of piling up.
 */
export function onRevive(key: string, listener: () => void) {
  listeners.set(key, listener)
}

/** The provider `fetch` for owallet. */
export const fetch = make({
  base: Norm.owalletUrl,
  reachable: (base) => Norm.probe(base),
  // Every window on this wallet notices at once. The lock lets one of them
  // start the serve; the rest then find it answering and start nothing.
  start: (base) => Flock.withLock(`norm-owallet-start:${new URL(base).host}`, () => Norm.startServer(base)),
  fetch: globalThis.fetch,
  revived: () => listeners.forEach((listener) => listener()),
})

export * as OwalletRevive from "./revive"
