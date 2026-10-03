import { Effect, Schema } from "effect"
import { SessionWake } from "@/norm/wake"
import * as Tool from "@/tool/tool"
import DESCRIPTION from "./schedule-wakeup.txt"

const MIN_DELAY = 60
const MAX_DELAY = 3600
// One pending wakeup per session: scheduling again replaces it.
const KEY = "wakeup"

// Names and shapes mirror Claude Code's ScheduleWakeup so models trained on
// it call this one the same way. Everything is optional in the schema because
// `stop: true` is sent alone; `execute` requires the rest otherwise.
export const Parameters = Schema.Struct({
  delaySeconds: Schema.optional(Schema.Number).annotate({
    description: "Seconds from now to wake up. Clamped to [60, 3600] by the runtime. Required unless `stop` is true.",
  }),
  noop: Schema.optional(Schema.Boolean).annotate({
    description:
      "true = nothing changed (you checked and there is nothing to report). false = something happened worth keeping (edited a file, posted a message, advanced state, surfaced a finding). Required unless `stop` is true.",
  }),
  prompt: Schema.optional(Schema.String).annotate({
    description:
      "The /loop input to fire on wake-up. Pass the same /loop input verbatim each turn so the next firing continues the loop. Required unless `stop` is true.",
  }),
  reason: Schema.optional(Schema.String).annotate({
    description:
      "One short sentence explaining the chosen delay. Shown to the user. Be specific. Required unless `stop` is true.",
  }),
  stop: Schema.optional(Schema.Boolean).annotate({
    description:
      "Set to true to end the dynamic loop immediately instead of scheduling another wakeup. When true, all other fields are ignored and no further wakeups fire.",
  }),
})

type Metadata = {
  stopped: boolean
  delaySeconds?: number
  at?: number
  noop?: boolean
  reason?: string
}

export const ScheduleWakeupTool = Tool.define<typeof Parameters, Metadata, SessionWake.Service>(
  "ScheduleWakeup",
  Effect.gen(function* () {
    const wake = yield* SessionWake.Service

    return {
      description: DESCRIPTION,
      parameters: Parameters,
      execute: (params: Schema.Schema.Type<typeof Parameters>, ctx: Tool.Context<Metadata>) =>
        Effect.gen(function* () {
          if (params.stop) {
            const pending = yield* wake.cancel(ctx.sessionID, KEY)
            return {
              title: "Loop stopped",
              output: pending
                ? "Loop stopped. The pending wakeup was cancelled and no further wakeups will fire."
                : "Loop stopped. No wakeup was pending.",
              metadata: { stopped: true },
            }
          }
          if (params.delaySeconds === undefined || !Number.isFinite(params.delaySeconds))
            return yield* Effect.fail(new Error("delaySeconds is required unless stop is true"))
          if (!params.prompt?.trim()) return yield* Effect.fail(new Error("prompt is required unless stop is true"))
          if (!params.reason?.trim()) return yield* Effect.fail(new Error("reason is required unless stop is true"))

          const delaySeconds = Math.round(Math.min(MAX_DELAY, Math.max(MIN_DELAY, params.delaySeconds)))
          const timer = yield* wake.whenIdle({
            sessionID: ctx.sessionID,
            key: KEY,
            delay: `${delaySeconds} seconds`,
            // The prompt shows in the transcript like the user's own; the
            // note tells the model where it came from.
            text: params.prompt,
            synthetic: false,
            hidden: `This message was sent by the wakeup you scheduled with ScheduleWakeup (${params.reason.trim()}), not typed by the user. Run the next iteration, then call ScheduleWakeup again to keep the loop going, or with stop: true to end it.`,
          })
          return {
            title: `Wake in ${duration(delaySeconds)}: ${params.reason.trim()}`,
            output: [
              `Wakeup scheduled in ${delaySeconds}s${delaySeconds === params.delaySeconds ? "" : ` (clamped from ${params.delaySeconds}s)`}.`,
              "The prompt will be sent to this session once the delay has passed and the session is idle.",
              "End your turn now; do not sleep or poll while waiting.",
            ].join(" "),
            metadata: {
              stopped: false,
              delaySeconds,
              at: timer.at,
              noop: params.noop,
              reason: params.reason.trim(),
            },
          }
        }).pipe(Effect.orDie),
    } satisfies Tool.DefWithoutID<typeof Parameters, Metadata>
  }),
)

function duration(seconds: number) {
  if (seconds % 60 === 0) return `${seconds / 60}m`
  if (seconds < 60) return `${seconds}s`
  return `${Math.floor(seconds / 60)}m ${seconds % 60}s`
}
