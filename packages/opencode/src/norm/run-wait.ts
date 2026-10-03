// norm: `norm run` used to exit the moment the session went idle. With a
// wakeup scheduled, a monitor armed or a background command running, idle is
// only a pause: something will start another turn, and (without --attach)
// exiting would kill it. This decides when a run is really over.

export type Pending = {
  wakeups: { key: string; at: number }[]
  tasks: { id: string; type: string; description: string }[]
}

type Get = (options: { url: string; path: Record<string, string> }) => Promise<{ data?: unknown }>

/** What will still start a turn in this session. The endpoint is norm's own,
 * so the generated SDK has no method for it; this goes through the SDK's
 * underlying HTTP client, as the TUI does for send_queued. */
export async function pending(client: unknown, sessionID: string): Promise<Pending> {
  const http = (client as { client: { get: Get } }).client
  const result = await http.get({ url: "/session/{sessionID}/pending", path: { sessionID } }).catch(() => undefined)
  const data = result?.data as Partial<Pending> | undefined
  return { wakeups: data?.wakeups ?? [], tasks: data?.tasks ?? [] }
}

export function describe(input: Pending) {
  const wake = input.wakeups.toSorted((a, b) => a.at - b.at)[0]
  return [
    ...(wake ? [`a wakeup at ${new Date(wake.at).toLocaleTimeString()}`] : []),
    ...(input.tasks.length ? [`${input.tasks.length} background ${input.tasks.length === 1 ? "task" : "tasks"}`] : []),
  ].join(" and ")
}

/**
 * Feed it every event of the run. `next` answers whether the run is over.
 *
 * A heartbeat (every 10 s) re-checks while waiting, for work that ends
 * without a turn (stopped from another client). It takes two quiet
 * heartbeats in a row: between a timer firing and its turn starting the
 * session is briefly idle with nothing pending.
 */
export function tracker(input: { check: () => Promise<Pending>; onWait: (pending: Pending) => void }) {
  const state = { idle: false, waiting: false, quiet: 0 }
  const busy = (found: Pending) => found.wakeups.length > 0 || found.tasks.length > 0
  return {
    async next(event: { type: "idle" | "busy" | "heartbeat" }): Promise<"done" | "continue"> {
      if (event.type === "busy") {
        state.idle = false
        state.quiet = 0
        return "continue"
      }
      if (event.type === "idle") {
        state.idle = true
        state.quiet = 0
        const found = await input.check()
        if (!busy(found)) return "done"
        if (!state.waiting) input.onWait(found)
        state.waiting = true
        return "continue"
      }
      if (!state.waiting || !state.idle) return "continue"
      if (busy(await input.check())) {
        state.quiet = 0
        return "continue"
      }
      state.quiet++
      return state.quiet >= 2 ? "done" : "continue"
    },
  }
}

export * as NormRunWait from "./run-wait"
