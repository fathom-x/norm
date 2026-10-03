import { LayerNode } from "@opencode-ai/core/effect/layer-node"
import { Clock, Context, Duration, Effect, Fiber, Layer, Schema, Scope } from "effect"
import { InstanceState } from "@/effect/instance-state"
import { SessionID } from "@/session/schema"
import { Session } from "@/session/session"
import { CronExpr } from "./cron-expr"
import { SessionWake } from "./wake"

const MAX_JOBS = 50
// A recurring job fires one last time at its first match past this age.
const MAX_AGE = Duration.toMillis(Duration.days(7))
// Jobs are spread out so that everyone's "hourly" does not hit the provider
// in the same second: recurring ones fire up to a tenth of their period late
// (at most 15 minutes), one-shots on :00 or :30 up to 90 seconds early. The
// amount is fixed per job, so a job keeps its rhythm.
const RECURRING_JITTER = 0.1
const RECURRING_JITTER_MAX = 15 * 60_000
const ONE_SHOT_JITTER = 90_000

export type Job = {
  id: string
  cron: string
  prompt: string
  recurring: boolean
  created: number
  /** When it fires next, jitter included. */
  next: number
}

export class LimitError extends Schema.TaggedErrorClass<LimitError>()("CronLimitError", {}) {
  override get message() {
    return `This session already has ${MAX_JOBS} scheduled jobs. Delete one with CronDelete first.`
  }
}

export class NeverError extends Schema.TaggedErrorClass<NeverError>()("CronNeverError", { cron: Schema.String }) {
  override get message() {
    return `The cron expression "${this.cron}" never matches a date.`
  }
}

export interface Interface {
  /** Throws (as a defect) on a malformed expression, with the reason. */
  readonly create: (input: {
    sessionID: SessionID
    cron: string
    prompt: string
    recurring: boolean
  }) => Effect.Effect<Job, LimitError | NeverError>
  readonly list: (sessionID: SessionID) => Effect.Effect<Job[]>
  readonly remove: (sessionID: SessionID, id: string) => Effect.Effect<boolean>
}

export class Service extends Context.Service<Service, Interface>()("@opencode/SessionCron") {}

type Entry = { job: Job; fiber: Fiber.Fiber<void> }

/** A stable fraction in [0, 1) for a job id. */
export function fraction(id: string) {
  let hash = 2166136261
  for (const char of id) hash = Math.imul(hash ^ char.charCodeAt(0), 16777619)
  return (hash >>> 0) / 2 ** 32
}

/** When the match at `fire` is acted on: `following` is the match after it. */
export function jittered(input: { id: string; recurring: boolean; fire: Date; following?: Date }) {
  const time = input.fire.getTime()
  if (input.recurring) {
    const period = input.following ? input.following.getTime() - time : 0
    return time + Math.floor(fraction(input.id) * Math.min(period * RECURRING_JITTER, RECURRING_JITTER_MAX))
  }
  const minute = input.fire.getMinutes()
  if (minute !== 0 && minute !== 30) return time
  return time - Math.floor(fraction(input.id) * ONE_SHOT_JITTER)
}

const layer = Layer.effect(
  Service,
  Effect.gen(function* () {
    const wake = yield* SessionWake.Service
    const sessions = yield* Session.Service

    // In memory, per instance: jobs are gone when the process exits or the
    // instance is disposed, as in Claude Code.
    const state = yield* InstanceState.make(
      Effect.fn("SessionCron.state")(function* () {
        return { scope: yield* Scope.Scope, jobs: new Map<SessionID, Map<string, Entry>>() }
      }),
    )

    const schedule = (job: Job, expr: CronExpr.CronExpr, now: number) => {
      const fire = CronExpr.next(expr, new Date(now))
      if (!fire) return
      const at = jittered({ id: job.id, recurring: job.recurring, fire, following: CronExpr.next(expr, fire) })
      return {
        // Early jitter must not put a one-shot in the past.
        at: Math.max(at, now),
        last: job.recurring && fire.getTime() >= job.created + MAX_AGE,
      }
    }

    const run = Effect.fn("SessionCron.run")(function* (sessionID: SessionID, job: Job, expr: CronExpr.CronExpr) {
      const data = yield* InstanceState.get(state)
      const forget = Effect.sync(() => {
        data.jobs.get(sessionID)?.delete(job.id)
      })
      while (true) {
        const now = yield* Clock.currentTimeMillis
        const due = schedule(job, expr, now)
        if (!due) return yield* forget
        job.next = due.at
        yield* Effect.sleep(Duration.millis(due.at - now))
        // A deleted session takes its jobs with it.
        if (!(yield* sessions.get(sessionID).pipe(Effect.isSuccess))) return yield* forget
        // Keyed per job: a fire still waiting for the session to go idle is
        // replaced by the next one, so missed fires collapse into one.
        yield* wake.whenIdle({
          sessionID,
          key: `cron:${job.id}`,
          delay: 0,
          text: job.prompt,
          synthetic: false,
          hidden: `This message was sent by the scheduled job ${job.id} (cron "${job.cron}") you created with CronCreate, not typed by the user.${due.last ? " This was its final run: recurring jobs expire after 7 days." : ""}`,
        })
        if (!job.recurring || due.last) return yield* forget
      }
    })

    const create: Interface["create"] = Effect.fn("SessionCron.create")(function* (input) {
      const data = yield* InstanceState.get(state)
      const expr = CronExpr.parse(input.cron)
      const jobs = data.jobs.get(input.sessionID) ?? new Map<string, Entry>()
      if (jobs.size >= MAX_JOBS) return yield* new LimitError()
      const created = yield* Clock.currentTimeMillis
      const job: Job = {
        id: crypto.randomUUID().replaceAll("-", "").slice(0, 8),
        cron: expr.source,
        prompt: input.prompt,
        recurring: input.recurring,
        created,
        next: created,
      }
      const first = schedule(job, expr, created)
      if (!first) return yield* new NeverError({ cron: expr.source })
      job.next = first.at
      const fiber = yield* run(input.sessionID, job, expr).pipe(Effect.forkIn(data.scope))
      data.jobs.set(input.sessionID, jobs.set(job.id, { job, fiber }))
      return { ...job }
    })

    const list: Interface["list"] = Effect.fn("SessionCron.list")(function* (sessionID) {
      const data = yield* InstanceState.get(state)
      return Array.from(data.jobs.get(sessionID)?.values() ?? [])
        .map((entry) => ({ ...entry.job }))
        .toSorted((a, b) => a.next - b.next)
    })

    const remove: Interface["remove"] = Effect.fn("SessionCron.remove")(function* (sessionID, id) {
      const data = yield* InstanceState.get(state)
      const entry = data.jobs.get(sessionID)?.get(id)
      if (!entry) return false
      data.jobs.get(sessionID)?.delete(id)
      yield* Fiber.interrupt(entry.fiber)
      yield* wake.cancel(sessionID, `cron:${id}`)
      return true
    })

    return Service.of({ create, list, remove })
  }),
)

export const node = LayerNode.make({ service: Service, layer, deps: [SessionWake.node, Session.node] })

export * as SessionCron from "./cron"
