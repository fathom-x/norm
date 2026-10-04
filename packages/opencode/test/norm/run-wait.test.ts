import { afterEach, describe, expect, test } from "bun:test"
import { LayerNode } from "@opencode-ai/core/effect/layer-node"
import { Effect, Layer } from "effect"
import { NormRunWait } from "@/norm/run-wait"
import { Session } from "@/session/session"
import { disposeAllInstances, TestInstance } from "../fixture/fixture"
import { testEffect } from "../lib/effect"
import { httpApiLayer, requestInDirectory } from "../server/httpapi-layer"

afterEach(async () => {
  await disposeAllInstances()
})

const nothing: NormRunWait.Pending = { wakeups: [], tasks: [] }
const wakeup: NormRunWait.Pending = { wakeups: [{ key: "wakeup", at: 1_800_000_000_000 }], tasks: [] }
// A monitor: always under a deadline, so a run waits for it.
const task: NormRunWait.Pending = {
  wakeups: [],
  tasks: [{ id: "mon_1", type: "monitor", description: "errors", deadline: 1_800_000_300_000 }],
}
// A background command started without a timeout: it may never exit.
const server: NormRunWait.Task = { id: "sh_1", type: "shell", description: "bun dev" }

// A tracker over a scripted sequence of what the server reports as pending.
function scripted(...reports: NormRunWait.Pending[]) {
  const announced: NormRunWait.Pending[] = []
  const left: NormRunWait.Task[][] = []
  const tracker = NormRunWait.tracker({
    check: async () => reports.shift() ?? nothing,
    onWait: (pending) => void announced.push(pending),
    onLeave: (tasks) => void left.push(tasks),
  })
  return { tracker, announced, left }
}

describe("norm run wait", () => {
  test("a turn with nothing pending ends the run, as before", async () => {
    const { tracker, announced } = scripted(nothing)
    expect(await tracker.next({ type: "busy" })).toBe("continue")
    expect(await tracker.next({ type: "idle" })).toBe("done")
    expect(announced).toEqual([])
  })

  test("a pending wakeup keeps the run going until the turn it starts has ended", async () => {
    const { tracker, announced } = scripted(wakeup, nothing)
    expect(await tracker.next({ type: "idle" })).toBe("continue")
    expect(announced).toEqual([wakeup])

    // The wakeup fires: a new turn runs, then nothing is left.
    expect(await tracker.next({ type: "busy" })).toBe("continue")
    expect(await tracker.next({ type: "heartbeat" })).toBe("continue")
    expect(await tracker.next({ type: "idle" })).toBe("done")
  })

  test("announces the wait once, however many turns it spans", async () => {
    const { tracker, announced } = scripted(task, task, nothing)
    await tracker.next({ type: "idle" })
    await tracker.next({ type: "busy" })
    expect(await tracker.next({ type: "idle" })).toBe("continue")
    expect(announced).toHaveLength(1)
    await tracker.next({ type: "busy" })
    expect(await tracker.next({ type: "idle" })).toBe("done")
  })

  test("work that ends without a turn is noticed on the second quiet heartbeat", async () => {
    // Stopped from another client: no turn follows, so no idle event either.
    const { tracker } = scripted(task, task, nothing, nothing)
    await tracker.next({ type: "idle" })
    expect(await tracker.next({ type: "heartbeat" })).toBe("continue")
    expect(await tracker.next({ type: "heartbeat" })).toBe("continue")
    expect(await tracker.next({ type: "heartbeat" })).toBe("done")
  })

  test("one quiet heartbeat is not enough: a firing timer looks the same for a moment", async () => {
    const { tracker } = scripted(wakeup, nothing, nothing)
    await tracker.next({ type: "idle" })
    expect(await tracker.next({ type: "heartbeat" })).toBe("continue")
    // The wakeup's turn starts before the next heartbeat.
    await tracker.next({ type: "busy" })
    expect(await tracker.next({ type: "heartbeat" })).toBe("continue")
    expect(await tracker.next({ type: "idle" })).toBe("done")
  })

  test("a background command with no timeout does not hold the run open", async () => {
    const { tracker, announced, left } = scripted({ wakeups: [], tasks: [server] })
    expect(await tracker.next({ type: "idle" })).toBe("done")
    expect(announced).toEqual([])
    // The caller is told what it is walking away from.
    expect(left).toEqual([[server]])
  })

  test("a missing deadline arrives as null over HTTP and still means no timeout", async () => {
    const wire = { ...server, deadline: null }
    const { tracker, left } = scripted({ wakeups: [], tasks: [wire] })
    expect(await tracker.next({ type: "idle" })).toBe("done")
    expect(left).toEqual([[wire]])
  })

  test("beside bounded work, it is waited out with it and then reported", async () => {
    const both = { wakeups: [], tasks: [...task.tasks, server] }
    const { tracker, announced, left } = scripted(both, { wakeups: [], tasks: [server] })
    expect(await tracker.next({ type: "idle" })).toBe("continue")
    expect(announced).toHaveLength(1)
    expect(left).toEqual([])

    await tracker.next({ type: "busy" })
    expect(await tracker.next({ type: "idle" })).toBe("done")
    expect(left).toEqual([[server]])
  })

  test("heartbeats before the first turn ends are ignored", async () => {
    const { tracker } = scripted()
    expect(await tracker.next({ type: "heartbeat" })).toBe("continue")
    expect(await tracker.next({ type: "heartbeat" })).toBe("continue")
  })

  test("says what it is waiting for", () => {
    expect(NormRunWait.describe(task)).toBe("1 background task")
    // Only what it is actually waiting for.
    expect(NormRunWait.describe({ ...task, tasks: [...task.tasks, server] })).toBe("1 background task")
    expect(NormRunWait.describe({ ...task, tasks: [...task.tasks, ...task.tasks] })).toBe("2 background tasks")
    const text = NormRunWait.describe({ ...wakeup, tasks: task.tasks })
    expect(text).toStartWith("a wakeup at ")
    expect(text).toEndWith(" and 1 background task")
  })

  test("an unreachable or older server reads as nothing pending", async () => {
    const failing = { client: { get: async () => Promise.reject(new Error("404")) } }
    expect(await NormRunWait.pending(failing, "ses_1")).toEqual(nothing)
    const older = { client: { get: async () => ({ data: undefined }) } }
    expect(await NormRunWait.pending(older, "ses_1")).toEqual(nothing)
  })
})

const it = testEffect(Layer.mergeAll(LayerNode.compile(Session.node), httpApiLayer))

describe("GET /session/:id/pending", () => {
  it.instance(
    "reports an idle session's pending work as empty lists",
    () =>
      Effect.gen(function* () {
        const dir = (yield* TestInstance).directory
        const session = yield* Effect.acquireRelease(Session.use.create({}), (created) =>
          Session.use.remove(created.id).pipe(Effect.ignore),
        )

        const res = yield* requestInDirectory(`/session/${session.id}/pending`, dir)
        expect(res.status).toBe(200)
        expect(yield* res.json).toEqual({ wakeups: [], tasks: [] })

        const missing = yield* requestInDirectory("/session/ses_missing/pending", dir)
        expect(missing.status).toBe(404)
      }),
    { git: true },
  )
})
