import { Duration, Effect, Schema } from "effect"
import { BackgroundTask } from "@/norm/background"
import { ShellTool } from "@/tool/shell"
import * as Tool from "@/tool/tool"
import DESCRIPTION from "./monitor.txt"

const DEFAULT_TIMEOUT = 300_000
const MAX_TIMEOUT = 1_800_000
const MIN_TIMEOUT = 1_000

// Mirrors Claude Code's Monitor (its WebSocket source is not implemented).
// `timeout_ms` is required there; here it may be left out, for models that
// were not trained on it.
export const Parameters = Schema.Struct({
  command: Schema.String.annotate({
    description: "Shell command or script. Each stdout line is an event; exit ends the watch.",
  }),
  description: Schema.String.annotate({
    description: "Short human-readable description of what you are monitoring (shown in notifications).",
  }),
  timeout_ms: Schema.optional(Schema.Number).annotate({
    description:
      "Kill the monitor after this deadline. Default 300000ms. Deadlines above 1800000ms are capped to 1800000ms. You are notified at expiry and can re-arm.",
  }),
})

type Metadata = {
  taskId: string
  outputFile: string
  timeoutMs: number
}

export const MonitorTool = Tool.define(
  "Monitor",
  Effect.gen(function* () {
    const tasks = yield* BackgroundTask.Service
    const shellInfo = yield* ShellTool

    return () =>
      Effect.gen(function* () {
        const shell = yield* Tool.init(shellInfo)

        return {
          description: DESCRIPTION,
          parameters: Parameters,
          execute: (params: Schema.Schema.Type<typeof Parameters>, ctx: Tool.Context<Metadata>) =>
            Effect.gen(function* () {
              if (!params.command.trim()) return yield* Effect.fail(new Error("command is required"))
              if (!params.description.trim()) return yield* Effect.fail(new Error("description is required"))
              const timeoutMs = Math.round(
                Math.min(MAX_TIMEOUT, Math.max(MIN_TIMEOUT, params.timeout_ms ?? DEFAULT_TIMEOUT)),
              )

              // The shell tool asks for permission and resolves the shell,
              // cwd and environment exactly as it does for a bash call, then
              // hands the command over instead of running it.
              const started: BackgroundTask.Task[] = []
              yield* shell.execute(
                { command: params.command },
                BackgroundTask.withDetach(ctx, (launch) =>
                  tasks
                    .monitor({
                      ...launch,
                      sessionID: ctx.sessionID,
                      description: params.description.trim(),
                      timeout: Duration.millis(timeoutMs),
                    })
                    .pipe(Effect.map((task) => void started.push(task))),
                ),
              )
              const task = started[0]
              if (!task) return yield* Effect.fail(new Error("The monitor was not started"))

              return {
                title: params.description.trim(),
                output: [
                  `Monitor started with task id ${task.id}.`,
                  "Each stdout line will arrive as a <task-notification> message; keep working or end your turn, and do not poll for it.",
                  `It is stopped after ${Math.round(timeoutMs / 1000)}s (re-arm it if you still need the watch), or earlier with TaskStop.`,
                  `Full output, stderr included: ${task.outputFile}`,
                ].join("\n"),
                metadata: { taskId: task.id, outputFile: task.outputFile, timeoutMs },
              }
            }).pipe(Effect.orDie),
        } satisfies Tool.DefWithoutID<typeof Parameters, Metadata>
      })
  }),
)
