import { LayerNode } from "@opencode-ai/core/effect/layer-node"
import { Database } from "@opencode-ai/core/database/database"
import { SessionV1 } from "@opencode-ai/core/v1/session"
import { Cause, Clock, Context, Duration, Effect, Fiber, Layer, Schedule, Scope } from "effect"
import { InstanceState } from "@/effect/instance-state"
import { MessageV2 } from "@/session/message-v2"
import { SessionRunState } from "@/session/run-state"
import type { SessionPrompt } from "@/session/prompt"
import { SessionID } from "@/session/schema"
import { Norm } from "./norm"

// How many extra runs `drive` starts when a delivered message is still
// unanswered after the run it joined. One covers the end-of-run race; the
// second is slack for a run that ended for an unrelated reason.
const RETRIES = 2

/** The one self-scheduled wakeup a session can have pending (ScheduleWakeup). */
export const WAKEUP = "wakeup"
// How long after a /loop iteration that forgot to reschedule the loop gets
// one more chance, as in Claude Code.
const FALLBACK_DELAY = Duration.minutes(20)

/**
 * `NORM_DISABLE_WAKE=1` leaves out everything built on this layer (the
 * scheduling and background tools, `/loop`, bash's `run_in_background`).
 * Unset, they follow the rest of the norm layer (`NORM_DISABLE`);
 * `NORM_DISABLE_WAKE=0` keeps them regardless.
 */
export function disabled() {
  const flag = process.env.NORM_DISABLE_WAKE
  if (flag === "1" || flag === "true") return true
  if (flag === "0" || flag === "false") return false
  return Norm.disabled()
}

/** The two SessionPrompt entry points the wake layer needs. */
export interface Ops {
  readonly prompt: (input: SessionPrompt.PromptInput) => Effect.Effect<SessionV1.WithParts>
  readonly loop: (input: { sessionID: SessionID }) => Effect.Effect<SessionV1.WithParts>
}

export type Pending = {
  key: string
  at: number
}

export type DeliverInput = {
  sessionID: SessionID
  text: string
  /** Hidden from the transcript but sent to the model. Defaults to true. */
  synthetic?: boolean
  /** Extra text for the model only, sent after `text` in the same message. */
  hidden?: string
}

export type WhenIdleInput = DeliverInput & {
  /** A new timer with the same key replaces the pending one. */
  key: string
  delay: Duration.Input
}

export interface Interface {
  /**
   * SessionPrompt hands over its entry points when its layer is built. It
   * cannot be a layer dependency: SessionPrompt depends on the tool registry,
   * and the tools depend on this service.
   */
  readonly attach: (ops: Ops) => Effect.Effect<void>
  /**
   * Persists a user message in the session and guarantees a model turn sees
   * it: a running turn picks it up at its next step, an idle session starts
   * one. Returns once the message is persisted, not when the turn ends.
   */
  readonly deliver: (input: DeliverInput) => Effect.Effect<void>
  /** Delivers after `delay`, waiting further until the session is idle. */
  readonly whenIdle: (input: WhenIdleInput) => Effect.Effect<Pending>
  /** Drops one pending timer. Returns whether there was one. */
  readonly cancel: (sessionID: SessionID, key: string) => Effect.Effect<boolean>
  /**
   * The session's run was cancelled: stop re-driving it. Timers survive,
   * because not every cancel is the user stopping the session (sending a
   * queued message at once cancels the reply too).
   */
  readonly interrupt: (sessionID: SessionID) => Effect.Effect<void>
  /** The user stopped the session: drop its pending timers and end its loop. */
  readonly clear: (sessionID: SessionID) => Effect.Effect<void>
  /**
   * A self-paced loop is running in the session with this prompt (set by
   * /loop and by every ScheduleWakeup). While one is, a turn that ends with
   * no wakeup pending gets a single fallback wakeup; if that iteration does
   * not reschedule either, the loop is over.
   */
  readonly loopStart: (sessionID: SessionID, prompt: string) => Effect.Effect<void>
  readonly loopStop: (sessionID: SessionID) => Effect.Effect<void>
  /** A run of the session ended. SessionPrompt calls this after every loop. */
  readonly settled: (sessionID: SessionID) => Effect.Effect<void>
  readonly pending: (sessionID: SessionID) => Effect.Effect<Pending[]>
}

export class Service extends Context.Service<Service, Interface>()("@opencode/SessionWake") {}

type Timer = Pending & { fiber: Fiber.Fiber<void> }

const layer = Layer.effect(
  Service,
  Effect.gen(function* () {
    const runs = yield* SessionRunState.Service
    const database = yield* Database.Service
    let attached: Ops | undefined

    const state = yield* InstanceState.make(
      Effect.fn("SessionWake.state")(function* () {
        return {
          // Timer and drive fibers live here, so disposing the instance stops them.
          scope: yield* Scope.Scope,
          timers: new Map<SessionID, Map<string, Timer>>(),
          // Bumped on every cancel. A cancelled run returns like a
          // finished one, so this is how `drive` tells them apart.
          epochs: new Map<SessionID, number>(),
          // `fallback`: the last wakeup was the fallback one, not the model's.
          loops: new Map<SessionID, { prompt: string; fallback: boolean }>(),
        }
      }),
    )

    const ops = Effect.suspend(() =>
      attached ? Effect.succeed(attached) : Effect.die(new Error("SessionWake used before SessionPrompt attached")),
    )

    const latest = Effect.fnUntraced(function* (sessionID: SessionID) {
      return MessageV2.latest(
        yield* MessageV2.filterCompactedEffect(sessionID).pipe(Effect.provideService(Database.Service, database)),
      )
    })

    // A prompt that arrives while a run is finishing joins that run and is
    // never read by it (the loop only re-reads messages at the top of a step),
    // so check afterwards and run again until the newest user message has a
    // reply.
    const drive: (sessionID: SessionID, epoch: number, attempt?: number) => Effect.Effect<void> = Effect.fn(
      "SessionWake.drive",
    )(function* (sessionID: SessionID, epoch: number, attempt = 0) {
      const data = yield* InstanceState.get(state)
      yield* (yield* ops).loop({ sessionID })
      if ((data.epochs.get(sessionID) ?? 0) !== epoch) return
      const last = yield* latest(sessionID)
      if (!last.user || last.assistant?.parentID === last.user.id) return
      if (attempt >= RETRIES) {
        yield* Effect.logWarning("delivered message got no turn", { "session.id": sessionID })
        return
      }
      yield* drive(sessionID, epoch, attempt + 1)
    })

    const deliver: Interface["deliver"] = Effect.fn("SessionWake.deliver")(function* (input) {
      const data = yield* InstanceState.get(state)
      // Keep the agent and model the session is already using; the defaults
      // would silently switch them.
      const user = (yield* latest(input.sessionID)).user
      yield* (yield* ops).prompt({
        sessionID: input.sessionID,
        agent: user?.agent,
        model: user ? { providerID: user.model.providerID, modelID: user.model.modelID } : undefined,
        variant: user?.model.variant,
        noReply: true,
        parts: [
          { type: "text", text: input.text, synthetic: input.synthetic ?? true },
          ...(input.hidden ? [{ type: "text" as const, text: input.hidden, synthetic: true }] : []),
        ],
      })
      yield* drive(input.sessionID, data.epochs.get(input.sessionID) ?? 0).pipe(
        Effect.catchCause((cause) =>
          Effect.logWarning("wake run failed", { "session.id": input.sessionID, cause: Cause.pretty(cause) }),
        ),
        Effect.forkIn(data.scope, { startImmediately: true }),
      )
    })

    const cancel: Interface["cancel"] = Effect.fn("SessionWake.cancel")(function* (sessionID, key) {
      const data = yield* InstanceState.get(state)
      const timer = data.timers.get(sessionID)?.get(key)
      if (!timer) return false
      data.timers.get(sessionID)?.delete(key)
      yield* Fiber.interrupt(timer.fiber)
      return true
    })

    const whenIdle: Interface["whenIdle"] = Effect.fn("SessionWake.whenIdle")(function* (input) {
      const data = yield* InstanceState.get(state)
      yield* cancel(input.sessionID, input.key)
      const at = (yield* Clock.currentTimeMillis) + Duration.toMillis(Duration.fromInputUnsafe(input.delay))
      const fiber = yield* Effect.sleep(input.delay).pipe(
        // session.status is not a reliable idle signal (it reports idle on
        // errors while the runner is still running), so ask the runner.
        Effect.andThen(runs.assertNotBusy(input.sessionID).pipe(Effect.retry(Schedule.spaced("1 second")))),
        Effect.andThen(
          Effect.suspend(() => {
            // Forget the timer first: from here on an interrupt must not
            // cancel a delivery that is already being written.
            if (data.timers.get(input.sessionID)?.get(input.key)?.at === at)
              data.timers.get(input.sessionID)?.delete(input.key)
            return deliver(input)
          }),
        ),
        // The session may be gone by the time the timer fires.
        Effect.catchCause((cause) =>
          Cause.hasInterruptsOnly(cause)
            ? Effect.void
            : Effect.logWarning("wake delivery failed", { "session.id": input.sessionID, cause: Cause.pretty(cause) }),
        ),
        Effect.forkIn(data.scope),
      )
      const timers = data.timers.get(input.sessionID) ?? new Map<string, Timer>()
      data.timers.set(input.sessionID, timers.set(input.key, { key: input.key, at, fiber }))
      return { key: input.key, at }
    })

    const interrupt: Interface["interrupt"] = Effect.fn("SessionWake.interrupt")(function* (sessionID) {
      const data = yield* InstanceState.get(state)
      data.epochs.set(sessionID, (data.epochs.get(sessionID) ?? 0) + 1)
    })

    const clear: Interface["clear"] = Effect.fn("SessionWake.clear")(function* (sessionID) {
      const data = yield* InstanceState.get(state)
      data.loops.delete(sessionID)
      const timers = Array.from(data.timers.get(sessionID)?.values() ?? [])
      data.timers.delete(sessionID)
      yield* Effect.forEach(timers, (timer) => Fiber.interrupt(timer.fiber), { discard: true })
    })

    const pending: Interface["pending"] = Effect.fn("SessionWake.pending")(function* (sessionID) {
      const data = yield* InstanceState.get(state)
      return Array.from(data.timers.get(sessionID)?.values() ?? [])
        .map((timer) => ({ key: timer.key, at: timer.at }))
        .toSorted((a, b) => a.at - b.at)
    })

    const loopStart: Interface["loopStart"] = Effect.fn("SessionWake.loopStart")(function* (sessionID, prompt) {
      const data = yield* InstanceState.get(state)
      data.loops.set(sessionID, { prompt, fallback: false })
    })

    const loopStop: Interface["loopStop"] = Effect.fn("SessionWake.loopStop")(function* (sessionID) {
      const data = yield* InstanceState.get(state)
      data.loops.delete(sessionID)
    })

    const settled: Interface["settled"] = Effect.fn("SessionWake.settled")(function* (sessionID) {
      const data = yield* InstanceState.get(state)
      const loop = data.loops.get(sessionID)
      if (!loop || data.timers.get(sessionID)?.has(WAKEUP)) return
      // The iteration the fallback started did not reschedule either.
      if (loop.fallback) {
        data.loops.delete(sessionID)
        return
      }
      loop.fallback = true
      yield* whenIdle({
        sessionID,
        key: WAKEUP,
        delay: FALLBACK_DELAY,
        text: loop.prompt,
        synthetic: false,
        hidden:
          "This message was sent by a fallback wakeup, not typed by the user: the previous iteration of this loop ended without calling ScheduleWakeup. Run the next iteration, then call ScheduleWakeup to keep the loop going, or with stop: true to end it. If you do neither, the loop ends here.",
      })
    })

    return Service.of({
      loopStart,
      loopStop,
      settled,
      attach: (input) =>
        Effect.sync(() => {
          attached = input
        }),
      deliver,
      whenIdle,
      cancel,
      interrupt,
      clear,
      pending,
    })
  }),
)

export const node = LayerNode.make({ service: Service, layer, deps: [SessionRunState.node, Database.node] })

export * as SessionWake from "./wake"
