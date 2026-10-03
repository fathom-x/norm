import { Effect } from "effect"
import * as Tool from "@/tool/tool"
import { SessionWake } from "./wake"
import { CronCreateTool, CronDeleteTool, CronListTool } from "./tool/cron"
import { MonitorTool } from "./tool/monitor"
import { ScheduleWakeupTool } from "./tool/schedule-wakeup"
import { TaskStopTool } from "./tool/task-stop"

/**
 * Tool ids offered to primary sessions only. A subagent runs inside its
 * parent's turn: one that schedules a wakeup or arms a monitor and ends its
 * turn would hand the parent a result that is not there yet.
 */
export function primaryOnly(): string[] {
  if (disabled()) return []
  return [ScheduleWakeupTool.id, MonitorTool.id, TaskStopTool.id, CronCreateTool.id, CronListTool.id, CronDeleteTool.id]
}

export const disabled = SessionWake.disabled

/** norm's own built-in tools, for the registry to initialise beside upstream's. */
export const infos = Effect.gen(function* () {
  const all: Tool.Info[] = disabled()
    ? []
    : [
        yield* ScheduleWakeupTool,
        yield* MonitorTool,
        yield* TaskStopTool,
        yield* CronCreateTool,
        yield* CronListTool,
        yield* CronDeleteTool,
      ]
  return all
})

export * as NormTools from "./tools"
