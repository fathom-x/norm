import { Effect, Schema } from "effect"
import { SessionCron } from "@/norm/cron"
import * as Tool from "@/tool/tool"
import DESCRIPTION_CREATE from "./cron-create.txt"

// These three mirror Claude Code's CronCreate, CronList and CronDelete.

export const CreateParameters = Schema.Struct({
  cron: Schema.String.annotate({
    description:
      'Standard 5-field cron expression in local time: "M H DoM Mon DoW" (e.g. "*/5 * * * *" = every 5 minutes, "30 14 28 2 *" = Feb 28 at 2:30pm local once).',
  }),
  prompt: Schema.String.annotate({ description: "The prompt to enqueue at each fire time." }),
  recurring: Schema.optional(Schema.Boolean).annotate({
    description:
      'true (default) = fire on every cron match until deleted or auto-expired after 7 days. false = fire once at the next match, then auto-delete. Use false for "remind me at X" one-shot requests with pinned minute/hour/dom/month.',
  }),
  durable: Schema.optional(Schema.Boolean).annotate({
    description:
      "Has no effect — durable persistence is not available. All jobs are session-only (in-memory, gone when this norm session ends).",
  }),
})

export const ListParameters = Schema.Struct({})

export const DeleteParameters = Schema.Struct({
  id: Schema.String.annotate({ description: "Job ID returned by CronCreate." }),
})

type JobMetadata = { id: string; next: number }

function when(time: number) {
  return new Date(time).toLocaleString()
}

export const CronCreateTool = Tool.define<typeof CreateParameters, JobMetadata, SessionCron.Service>(
  "CronCreate",
  Effect.gen(function* () {
    const cron = yield* SessionCron.Service

    return {
      description: DESCRIPTION_CREATE,
      parameters: CreateParameters,
      execute: (params: Schema.Schema.Type<typeof CreateParameters>, ctx: Tool.Context<JobMetadata>) =>
        Effect.gen(function* () {
          if (!params.prompt.trim()) return yield* Effect.fail(new Error("prompt is required"))
          const recurring = params.recurring ?? true
          const job = yield* cron.create({
            sessionID: ctx.sessionID,
            cron: params.cron,
            prompt: params.prompt,
            recurring,
          })
          return {
            title: `${recurring ? "Every" : "Once"} ${job.cron}`,
            output: recurring
              ? `Scheduled recurring job ${job.id} (${job.cron}). Next run: ${when(job.next)}. It fires while the session is idle, is gone when norm exits, and auto-expires after 7 days. Use CronDelete to cancel it sooner.`
              : `Scheduled one-shot job ${job.id} (${job.cron}). It will run once at ${when(job.next)}, while the session is idle, then delete itself. It is gone if norm exits first.`,
            metadata: { id: job.id, next: job.next },
          }
        }).pipe(Effect.orDie),
    } satisfies Tool.DefWithoutID<typeof CreateParameters, JobMetadata>
  }),
)

export const CronListTool = Tool.define<typeof ListParameters, { count: number }, SessionCron.Service>(
  "CronList",
  Effect.gen(function* () {
    const cron = yield* SessionCron.Service

    return {
      description: "List all cron jobs scheduled via CronCreate in this session.",
      parameters: ListParameters,
      execute: (_params: Schema.Schema.Type<typeof ListParameters>, ctx: Tool.Context<{ count: number }>) =>
        Effect.gen(function* () {
          const jobs = yield* cron.list(ctx.sessionID)
          return {
            title: `${jobs.length} scheduled ${jobs.length === 1 ? "job" : "jobs"}`,
            output:
              jobs.length === 0
                ? "No scheduled jobs."
                : jobs
                    .map(
                      (job) =>
                        `${job.id} — ${job.cron} (${job.recurring ? "recurring" : "one-shot"}), next run ${when(job.next)}: ${job.prompt}`,
                    )
                    .join("\n"),
            metadata: { count: jobs.length },
          }
        }),
    } satisfies Tool.DefWithoutID<typeof ListParameters, { count: number }>
  }),
)

export const CronDeleteTool = Tool.define<typeof DeleteParameters, { id: string }, SessionCron.Service>(
  "CronDelete",
  Effect.gen(function* () {
    const cron = yield* SessionCron.Service

    return {
      description:
        "Cancel a cron job previously scheduled with CronCreate. Removes it from the in-memory session store.",
      parameters: DeleteParameters,
      execute: (params: Schema.Schema.Type<typeof DeleteParameters>, ctx: Tool.Context<{ id: string }>) =>
        Effect.gen(function* () {
          const id = params.id.trim()
          if (!(yield* cron.remove(ctx.sessionID, id)))
            return yield* Effect.fail(new Error(`No scheduled job found with ID: ${id}`))
          return { title: `Cancelled ${id}`, output: `Cancelled job ${id}.`, metadata: { id } }
        }).pipe(Effect.orDie),
    } satisfies Tool.DefWithoutID<typeof DeleteParameters, { id: string }>
  }),
)
