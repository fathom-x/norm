import { Effect, Schema } from "effect"
import { BackgroundTask } from "@/norm/background"
import * as Tool from "@/tool/tool"
import DESCRIPTION from "./task-stop.txt"

// Mirrors Claude Code's TaskStop, deprecated `shell_id` alias included.
export const Parameters = Schema.Struct({
  task_id: Schema.optional(Schema.String).annotate({ description: "The ID of the background task to stop." }),
  shell_id: Schema.optional(Schema.String).annotate({ description: "Deprecated: use task_id instead" }),
})

type Metadata = {
  taskId: string
}

export const TaskStopTool = Tool.define<typeof Parameters, Metadata, BackgroundTask.Service>(
  "TaskStop",
  Effect.gen(function* () {
    const tasks = yield* BackgroundTask.Service

    return {
      description: DESCRIPTION,
      parameters: Parameters,
      execute: (params: Schema.Schema.Type<typeof Parameters>, ctx: Tool.Context<Metadata>) =>
        Effect.gen(function* () {
          const id = (params.task_id ?? params.shell_id)?.trim()
          if (!id) return yield* Effect.fail(new Error("task_id is required"))
          const stopped = yield* tasks.stop(ctx.sessionID, id)
          if (!stopped) return yield* Effect.fail(new Error(`No running task found with ID: ${id}`))
          return {
            title: `Stopped ${stopped.title ?? id}`,
            output: `Successfully stopped task: ${id}${stopped.title ? ` (${stopped.title})` : ""}`,
            metadata: { taskId: id },
          }
        }).pipe(Effect.orDie),
    } satisfies Tool.DefWithoutID<typeof Parameters, Metadata>
  }),
)
