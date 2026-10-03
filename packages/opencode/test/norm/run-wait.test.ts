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
const task: NormRunWait.Pending = { wakeups: [], tasks: [{ id: "mon_1", type: "monitor", description: "errors" }] }

// A tracker over a scripted sequence of what the server reports as pending.
function scripted(...reports: NormRunWait.Pending[]) {
  const announced: NormRunWait.Pending[] = []
  const tracker = NormRunWait.tracker({
    check: async () => reports.shift() ?? nothing,
    onWait: (pending) => announced.push(pending),
  })
  return { tracker, announced }
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

  test("heartbeats before the first turn ends are ignored", async () => {
    const { tracker } = scripted()
    expect(await tracker.next({ type: "heartbeat" })).toBe("continue")
    expect(await tracker.next({ type: "heartbeat" })).toBe("continue")
  })

  test("says what it is waiting for", () => {
    expect(NormRunWait.describe(task)).toBe("1 background task")
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
