// norm: `norm run` used to exit the moment the session went idle. With a
// wakeup scheduled, a monitor armed or a background command running, idle is
// only a pause: something will start another turn, and (without --attach)
// exiting would kill it. This decides when a run is really over.
//
// It waits only for work that is certain to end: wakeups, monitors (always
// under a deadline) and background commands started with a timeout. A
// background command with no timeout may be a server that never exits;
// waiting on it would hang the run forever.

// `deadline` arrives as null over HTTP when a task has none.
export type Task = { id: string; type: string; description: string; deadline?: number | null }

export type Pending = {
  wakeups: { key: string; at: number }[]
  tasks: Task[]
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

/** The tasks a run waits for: those that are killed at a known time. */
export function bounded(input: Pending) {
  return input.tasks.filter((task) => typeof task.deadline === "number")
}

/** The tasks a run does not wait for. */
export function unbounded(input: Pending) {
  return input.tasks.filter((task) => typeof task.deadline !== "number")
}

export function describe(input: Pending) {
  const wake = input.wakeups.toSorted((a, b) => a.at - b.at)[0]
  const tasks = bounded(input).length
  return [
    ...(wake ? [`a wakeup at ${new Date(wake.at).toLocaleTimeString()}`] : []),
    ...(tasks ? [`${tasks} background ${tasks === 1 ? "task" : "tasks"}`] : []),
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
export function tracker(input: {
  check: () => Promise<Pending>
  onWait: (pending: Pending) => void
  /** The run is ending with these still running, because they have no timeout. */
  onLeave: (tasks: Task[]) => void
}) {
  const state = { idle: false, waiting: false, quiet: 0 }
  const busy = (found: Pending) => found.wakeups.length > 0 || bounded(found).length > 0
  const done = (found: Pending) => {
    const left = unbounded(found)
    if (left.length > 0) input.onLeave(left)
    return "done" as const
  }
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
        if (!busy(found)) return done(found)
        if (!state.waiting) input.onWait(found)
        state.waiting = true
        return "continue"
      }
      if (!state.waiting || !state.idle) return "continue"
      const found = await input.check()
      if (busy(found)) {
        state.quiet = 0
        return "continue"
      }
      state.quiet++
      return state.quiet >= 2 ? done(found) : "continue"
    },
  }
}

export * as NormRunWait from "./run-wait"
