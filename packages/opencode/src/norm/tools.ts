import { Effect } from "effect"
import * as Tool from "@/tool/tool"
import { Norm } from "./norm"
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
  return [ScheduleWakeupTool.id, MonitorTool.id, TaskStopTool.id]
}

/**
 * `NORM_DISABLE_WAKE=1` leaves these tools out. Unset, they follow the rest
 * of the layer (`NORM_DISABLE`); `NORM_DISABLE_WAKE=0` keeps them regardless.
 */
export function disabled() {
  const flag = process.env.NORM_DISABLE_WAKE
  if (flag === "1" || flag === "true") return true
  if (flag === "0" || flag === "false") return false
  return Norm.disabled()
}

/** norm's own built-in tools, for the registry to initialise beside upstream's. */
export const infos = Effect.gen(function* () {
  const all: Tool.Info[] = disabled() ? [] : [yield* ScheduleWakeupTool, yield* MonitorTool, yield* TaskStopTool]
  return all
})

export * as NormTools from "./tools"
