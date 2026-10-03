import { afterEach, describe, expect } from "bun:test"
import { SessionV1 } from "@opencode-ai/core/v1/session"
import { Database } from "@opencode-ai/core/database/database"
import { LayerNode } from "@opencode-ai/core/effect/layer-node"
import { ModelV2 } from "@opencode-ai/core/model"
import { ProviderV2 } from "@opencode-ai/core/provider"
import { SessionProjector } from "@opencode-ai/core/session/projector"
import { Deferred, Effect, Scope } from "effect"
import { BackgroundJob } from "@/background/job"
import { RuntimeFlags } from "@/effect/runtime-flags"
import { EventV2Bridge } from "@/event-v2-bridge"
import { SessionWake } from "@/norm/wake"
import { MessageV2 } from "@/session/message-v2"
import { SessionRunState } from "@/session/run-state"
import { MessageID, PartID, SessionID } from "@/session/schema"
import { Session } from "@/session/session"
import { SessionStatus } from "@/session/status"
import { disposeAllInstances } from "../fixture/fixture"
import { testEffect } from "../lib/effect"

afterEach(async () => {
  await disposeAllInstances()
})

const it = testEffect(
  LayerNode.compile(
    LayerNode.group([
      BackgroundJob.node,
      EventV2Bridge.node,
      Session.node,
      SessionProjector.node,
      SessionRunState.node,
      SessionStatus.node,
      SessionWake.node,
      Database.node,
      RuntimeFlags.node,
    ]),
  ),
)

const ref = {
  providerID: ProviderV2.ID.make("test"),
  modelID: ModelV2.ID.make("test-model"),
}

// Stands in for SessionPrompt with the two properties the wake layer relies
// on: `prompt` persists a user message, and a run reads the session once when
// it starts, then replies to the newest user message it saw. Runs go through
// the real SessionRunState, so joining an in-flight run behaves as it does in
// the app.
const harness = Effect.fn("WakeTest.harness")(function* (options?: { reply?: boolean }) {
  const sessions = yield* Session.Service
  const runs = yield* SessionRunState.Service
  const wake = yield* SessionWake.Service
  const scope = yield* Scope.Scope
  const chat = yield* sessions.create({ title: "wake" })
  const state = {
    loops: 0,
    // Runs that have read the session.
    reads: 0,
    // Text of the user message each finished run replied to.
    replied: [] as string[],
    // When set, a run waits here after reading the session.
    hold: undefined as Deferred.Deferred<void> | undefined,
  }

  const read = Effect.fnUntraced(function* () {
    return yield* sessions.messages({ sessionID: chat.id }).pipe(Effect.orDie)
  })

  const assistant = (parentID: MessageID): SessionV1.Assistant => ({
    id: MessageID.ascending(),
    role: "assistant",
    parentID,
    sessionID: chat.id,
    mode: "build",
    agent: "build",
    cost: 0,
    path: { cwd: "/tmp", root: "/tmp" },
    tokens: { input: 0, output: 0, reasoning: 0, cache: { read: 0, write: 0 } },
    modelID: ref.modelID,
    providerID: ref.providerID,
    time: { created: Date.now() },
    finish: "stop",
  })

  const prompt: SessionWake.Ops["prompt"] = Effect.fnUntraced(function* (input) {
    const info = yield* sessions.updateMessage({
      id: MessageID.ascending(),
      role: "user",
      sessionID: input.sessionID,
      agent: input.agent ?? "default-agent",
      model: { ...(input.model ?? ref), variant: input.variant },
      time: { created: Date.now() },
    })
    const parts = yield* Effect.forEach(input.parts, (part) =>
      part.type === "text"
        ? sessions.updatePart({
            id: PartID.ascending(),
            messageID: info.id,
            sessionID: input.sessionID,
            type: "text",
            text: part.text,
            synthetic: part.synthetic,
          })
        : Effect.die(new Error("unexpected part")),
    )
    return { info, parts }
  })

  const run = Effect.gen(function* () {
    const seen = yield* read()
    const user = MessageV2.latest(seen).user
    state.reads++
    if (state.hold) yield* Deferred.await(state.hold)
    if (!user || options?.reply === false) return { info: assistant(MessageID.ascending()), parts: [] }
    const info = yield* sessions.updateMessage(assistant(user.id))
    state.replied.push(text(seen.find((message) => message.info.id === user.id)))
    return { info, parts: [] }
  })

  const loop: SessionWake.Ops["loop"] = (input) =>
    Effect.suspend(() => {
      state.loops++
      return runs.ensureRunning(
        input.sessionID,
        Effect.sync(() => ({ info: assistant(MessageID.ascending()), parts: [] })),
        run,
      )
    })

  yield* wake.attach({ prompt, loop })

  return {
    chat,
    state,
    wake,
    runs,
    read,
    // A user turn that is held open until `release` is called.
    startHeld: Effect.fnUntraced(function* (message: string) {
      const hold = yield* Deferred.make<void>()
      state.hold = hold
      yield* prompt({ sessionID: chat.id, agent: "build", model: ref, parts: [{ type: "text", text: message }] })
      yield* loop({ sessionID: chat.id }).pipe(Effect.forkIn(scope, { startImmediately: true }))
      const reads = state.reads
      yield* eventually(() => Effect.sync(() => state.reads > reads))
      return Effect.suspend(() => {
        state.hold = undefined
        return Deferred.succeed(hold, undefined)
      })
    }),
  }
})

function text(message?: SessionV1.WithParts) {
  return message?.parts.flatMap((part) => (part.type === "text" ? [part.text] : [])).join("") ?? ""
}

const eventually = Effect.fnUntraced(function* (check: () => Effect.Effect<boolean>, timeout = 3_000) {
  const deadline = Date.now() + timeout
  while (!(yield* check())) {
    if (Date.now() > deadline) throw new Error("condition not met in time")
    yield* Effect.sleep("10 millis")
  }
})

describe("SessionWake", () => {
  it.instance("delivering to an idle session starts a turn that answers it", () =>
    Effect.gen(function* () {
      const test = yield* harness()
      yield* test.wake.deliver({ sessionID: test.chat.id, text: "the build finished" })
      yield* eventually(() => Effect.sync(() => test.state.replied.length === 1))

      expect(test.state.replied).toEqual(["the build finished"])
      const user = (yield* test.read()).find((message) => message.info.role === "user")
      expect(user?.parts).toMatchObject([{ type: "text", text: "the build finished", synthetic: true }])
    }),
  )

  it.instance("a delivered message keeps the session's agent and model", () =>
    Effect.gen(function* () {
      const test = yield* harness()
      const release = yield* test.startHeld("hello")
      yield* release
      yield* eventually(() => Effect.sync(() => test.state.replied.length === 1))

      yield* test.wake.deliver({ sessionID: test.chat.id, text: "event", synthetic: false })
      yield* eventually(() => Effect.sync(() => test.state.replied.length === 2))

      const user = MessageV2.latest(yield* test.read()).user
      expect(user?.agent).toBe("build")
      expect(user?.model).toMatchObject(ref)
      const parts = (yield* test.read()).find((message) => message.info.id === user?.id)?.parts
      expect(parts).toMatchObject([{ type: "text", text: "event" }])
      expect(parts?.[0]).not.toMatchObject({ synthetic: true })
    }),
  )

  it.instance("a message delivered to a run that already read the session still gets a turn", () =>
    Effect.gen(function* () {
      const test = yield* harness()
      const release = yield* test.startHeld("hello")

      // The run has read its messages and will finish without seeing this one.
      yield* test.wake.deliver({ sessionID: test.chat.id, text: "late event" })
      yield* release
      yield* eventually(() => Effect.sync(() => test.state.replied.length === 2))

      expect(test.state.replied).toEqual(["hello", "late event"])
      // The user's loop, the joined loop, and one more run for the late message.
      expect(test.state.loops).toBe(3)
    }),
  )

  it.instance("an interrupt stops a delivery from restarting the session", () =>
    Effect.gen(function* () {
      const test = yield* harness()
      yield* test.startHeld("hello")
      yield* test.wake.deliver({ sessionID: test.chat.id, text: "late event" })
      yield* eventually(() => Effect.sync(() => test.state.loops === 2))

      // What SessionPrompt.cancel does on Esc.
      yield* test.wake.interrupt(test.chat.id)
      yield* test.runs.cancel(test.chat.id)
      yield* Effect.sleep("100 millis")

      expect(test.state.loops).toBe(2)
      expect(test.state.replied).toEqual([])
      yield* test.runs.assertNotBusy(test.chat.id)
    }),
  )

  it.instance("gives up when runs keep ending without answering", () =>
    Effect.gen(function* () {
      const test = yield* harness({ reply: false })
      yield* test.wake.deliver({ sessionID: test.chat.id, text: "event" })
      yield* eventually(() => Effect.sync(() => test.state.loops === 3))
      yield* Effect.sleep("100 millis")

      expect(test.state.loops).toBe(3)
    }),
  )

  it.instance("whenIdle delivers after the delay and reports what is pending", () =>
    Effect.gen(function* () {
      const test = yield* harness()
      const before = Date.now()
      const timer = yield* test.wake.whenIdle({
        sessionID: test.chat.id,
        key: "wakeup",
        delay: "150 millis",
        text: "wake up",
      })

      expect(timer.key).toBe("wakeup")
      expect(timer.at).toBeGreaterThanOrEqual(before + 150)
      expect(yield* test.wake.pending(test.chat.id)).toEqual([timer])
      expect(test.state.replied).toEqual([])

      yield* eventually(() => Effect.sync(() => test.state.replied.length === 1))
      expect(Date.now() - before).toBeGreaterThanOrEqual(150)
      expect(test.state.replied).toEqual(["wake up"])
      expect(yield* test.wake.pending(test.chat.id)).toEqual([])
    }),
  )

  it.instance("a timer with the same key replaces the pending one", () =>
    Effect.gen(function* () {
      const test = yield* harness()
      yield* test.wake.whenIdle({ sessionID: test.chat.id, key: "wakeup", delay: "50 millis", text: "first" })
      yield* test.wake.whenIdle({ sessionID: test.chat.id, key: "wakeup", delay: "50 millis", text: "second" })
      yield* test.wake.whenIdle({ sessionID: test.chat.id, key: "other", delay: "50 millis", text: "third" })
      expect((yield* test.wake.pending(test.chat.id)).map((timer) => timer.key).toSorted()).toEqual(["other", "wakeup"])

      // Both are due together; whichever loses waits for the other's turn to end.
      yield* eventually(() => Effect.sync(() => test.state.replied.length === 2))
      const delivered = (yield* test.read()).filter((message) => message.info.role === "user").map(text)
      expect(delivered.toSorted()).toEqual(["second", "third"])
    }),
  )

  it.instance("cancel and clear drop pending timers", () =>
    Effect.gen(function* () {
      const test = yield* harness()
      const other = yield* (yield* Session.Service).create({ title: "other" })
      yield* test.wake.whenIdle({ sessionID: test.chat.id, key: "a", delay: "50 millis", text: "a" })
      yield* test.wake.whenIdle({ sessionID: test.chat.id, key: "b", delay: "50 millis", text: "b" })
      yield* test.wake.whenIdle({ sessionID: other.id, key: "a", delay: "1 hour", text: "other" })

      expect(yield* test.wake.cancel(test.chat.id, "a")).toBe(true)
      expect(yield* test.wake.cancel(test.chat.id, "a")).toBe(false)
      yield* test.wake.clear(test.chat.id)
      expect(yield* test.wake.pending(test.chat.id)).toEqual([])
      yield* Effect.sleep("150 millis")

      expect(yield* test.read()).toEqual([])
      // Another session's timers are untouched.
      expect((yield* test.wake.pending(other.id)).map((timer) => timer.key)).toEqual(["a"])
    }),
  )

  it.instance("whenIdle waits for a busy session to go idle", () =>
    Effect.gen(function* () {
      const test = yield* harness()
      const release = yield* test.startHeld("hello")
      yield* test.wake.whenIdle({ sessionID: test.chat.id, key: "wakeup", delay: "10 millis", text: "wake up" })
      yield* Effect.sleep("200 millis")

      // Due, but the session is mid-turn: nothing is written yet.
      expect((yield* test.read()).filter((message) => message.info.role === "user")).toHaveLength(1)
      expect((yield* test.wake.pending(test.chat.id)).map((timer) => timer.key)).toEqual(["wakeup"])

      yield* release
      yield* eventually(() => Effect.sync(() => test.state.replied.length === 2))
      expect(test.state.replied).toEqual(["hello", "wake up"])
    }),
  )

  it.instance("a timer for a deleted session fires harmlessly", () =>
    Effect.gen(function* () {
      const test = yield* harness()
      yield* test.wake.whenIdle({ sessionID: test.chat.id, key: "wakeup", delay: "50 millis", text: "wake up" })
      yield* (yield* Session.Service).remove(test.chat.id)
      yield* Effect.sleep("200 millis")

      expect(test.state.replied).toEqual([])
      expect(yield* test.wake.pending(SessionID.make(test.chat.id))).toEqual([])
    }),
  )
})
