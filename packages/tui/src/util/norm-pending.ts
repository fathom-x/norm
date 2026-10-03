// norm: what will still start a turn in a session while it sits idle: a
// scheduled wakeup, a monitor, a background command (GET
// /session/:id/pending). The prompt's hints row shows it, so a quiet session
// that is going to act on its own does not look finished.

export type Pending = {
  wakeups: { key: string; at: number }[]
  tasks: { id: string; type: string; description: string }[]
}

type Get = (options: { url: string; path: Record<string, string> }) => Promise<{ data?: unknown }>

/** The endpoint is norm's own, so the generated SDK has no method for it;
 * this goes through the SDK's underlying HTTP client (see norm-queue.ts). */
export async function fetchPending(client: unknown, sessionID: string): Promise<Pending> {
  const http = (client as { client: { get: Get } }).client
  const result = await http.get({ url: "/session/{sessionID}/pending", path: { sessionID } }).catch(() => undefined)
  const data = result?.data as Partial<Pending> | undefined
  return { wakeups: data?.wakeups ?? [], tasks: data?.tasks ?? [] }
}

/** "wake 4m · 2 bg", or nothing when nothing is pending. */
export function pendingLabel(pending: Pending | undefined, now: number): string | undefined {
  if (!pending) return
  const at = pending.wakeups.map((item) => item.at).toSorted((a, b) => a - b)[0]
  const minutes = at === undefined ? undefined : Math.ceil((at - now) / 60_000)
  const parts = [
    ...(minutes === undefined ? [] : [minutes <= 1 ? "wake <1m" : `wake ${minutes}m`]),
    ...(pending.tasks.length ? [`${pending.tasks.length} bg`] : []),
  ]
  return parts.length ? parts.join(" · ") : undefined
}
