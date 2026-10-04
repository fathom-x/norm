import { createWriteStream } from "node:fs"
import path from "path"
import { LayerNode } from "@opencode-ai/core/effect/layer-node"
import { CrossSpawnSpawner } from "@opencode-ai/core/cross-spawn-spawner"
import { FSUtil } from "@opencode-ai/core/fs-util"
import { Cause, Clock, Context, Duration, Effect, Fiber, Layer, Queue, Stream } from "effect"
import { ChildProcess } from "effect/unstable/process"
import { ChildProcessSpawner } from "effect/unstable/process/ChildProcessSpawner"
import { BackgroundJob } from "@/background/job"
import { SessionID } from "@/session/schema"
import * as Tool from "@/tool/tool"
import { TRUNCATION_DIR } from "@/tool/truncation-dir"
import { SessionWake } from "./wake"

// Lines that arrive this close together become one notification, so the
// several lines one event prints stay together.
const BATCH = Duration.millis(200)
// More notifications than this in a minute stops the monitor: each one is a
// message in the conversation and can start a model turn.
const NOISE_LIMIT = 30
const NOISE_WINDOW = 60_000
// A notification carries at most this much output; the rest is in the file.
const EVENT_MAX = 8_000

/** A command the shell tool has already vetted: permission asked, cwd and env resolved. */
export type Launch = {
  command: string
  process: ChildProcess.Command
}

export type Task = {
  id: string
  type: "monitor"
  description: string
  command: string
  outputFile: string
}

export type MonitorInput = Launch & {
  sessionID: SessionID
  description: string
  timeout: Duration.Duration
}

export type Notification = {
  id: string
  summary: string
  status?: "completed" | "failed" | "killed"
  outputFile?: string
  event?: string
}

export interface Interface {
  /**
   * Runs the command outside any turn. Each batch of stdout lines is delivered
   * to the session as a notification; stderr only reaches the output file.
   */
  readonly monitor: (input: MonitorInput) => Effect.Effect<Task>
  /** Stops a task the session started. Returns it, or nothing if it has none running by that id. */
  readonly stop: (sessionID: SessionID, id: string) => Effect.Effect<BackgroundJob.Info | undefined>
  /** The session's running tasks. */
  readonly list: (sessionID: SessionID) => Effect.Effect<Task[]>
}

export class Service extends Context.Service<Service, Interface>()("@opencode/BackgroundTask") {}

// The shell tool hands its vetted command to this instead of running it when
// the caller put one in the tool context: that is how Monitor reuses the
// shell tool's permission checks, path resolution and environment.
const DETACH = "normDetach"

export type Detach = (launch: Launch) => Effect.Effect<void>

export function detach(ctx: Tool.Context) {
  const handler = ctx.extra?.[DETACH]
  return typeof handler === "function" ? (handler as Detach) : undefined
}

export function withDetach(ctx: Tool.Context, handler: Detach): Tool.Context {
  return { ...ctx, extra: { ...ctx.extra, [DETACH]: handler } }
}

/** The message a model sees for a background event, in Claude Code's format. */
export function notification(input: Notification) {
  return [
    "<task-notification>",
    `<task-id>${input.id}</task-id>`,
    ...(input.outputFile ? [`<output-file>${input.outputFile}</output-file>`] : []),
    ...(input.status ? [`<status>${input.status}</status>`] : []),
    `<summary>${input.summary}</summary>`,
    ...(input.event === undefined ? [] : [`<event>${input.event}</event>`]),
    "</task-notification>",
  ].join("\n")
}

const layer = Layer.effect(
  Service,
  Effect.gen(function* () {
    const jobs = yield* BackgroundJob.Service
    const wake = yield* SessionWake.Service
    const spawner = yield* ChildProcessSpawner
    const fs = yield* FSUtil.Service

    const deliver = (sessionID: SessionID, input: Notification) =>
      wake.deliver({ sessionID, text: notification(input) }).pipe(
        // The session may have been deleted under a running task.
        Effect.catchCause((cause) =>
          Effect.logWarning("task notification failed", { "session.id": sessionID, cause: Cause.pretty(cause) }),
        ),
      )

    const watch = Effect.fn("BackgroundTask.watch")(function* (input: MonitorInput, task: Task) {
      const handle = yield* spawner.spawn(input.process).pipe(Effect.orDie)
      // Kills the whole process group, also when the task is stopped or the
      // instance is disposed mid-run.
      yield* Effect.addFinalizer(() => handle.kill({ forceKillAfter: "3 seconds" }).pipe(Effect.ignore))
      const sink = yield* Effect.acquireRelease(
        Effect.sync(() => createWriteStream(task.outputFile, { flags: "a" })),
        (stream) => Effect.promise(() => new Promise<void>((resolve) => stream.end(resolve))),
      )

      const lines: string[] = []
      const arrived = yield* Queue.sliding<void>(1)
      const sent: number[] = []

      yield* Stream.runForEach(Stream.decodeText(handle.stderr), (chunk) => Effect.sync(() => sink.write(chunk))).pipe(
        Effect.ignore,
        Effect.forkScoped,
      )
      const reader = yield* Stream.decodeText(handle.stdout).pipe(
        Stream.tap((chunk) => Effect.sync(() => sink.write(chunk))),
        Stream.splitLines,
        Stream.runForEach((line) =>
          Effect.suspend(() => {
            lines.push(line)
            return Queue.offer(arrived, undefined)
          }),
        ),
        Effect.ignore,
        Effect.forkScoped,
      )

      // Never interrupted between taking the lines and delivering them, so a
      // race ending the watch cannot lose a batch.
      const flush = Effect.suspend(() => {
        if (lines.length === 0) return Effect.void
        const text = lines.splice(0).join("\n")
        const event =
          text.length > EVENT_MAX
            ? `${text.slice(0, EVENT_MAX)}\n[${text.length - EVENT_MAX} more characters; full output in ${task.outputFile}]`
            : text
        return Clock.currentTimeMillis.pipe(
          Effect.tap((now) => Effect.sync(() => sent.push(now))),
          Effect.andThen(
            deliver(input.sessionID, { id: task.id, summary: `Monitor event: "${input.description}"`, event }),
          ),
        )
      }).pipe(Effect.uninterruptible)

      const noisy = Effect.gen(function* () {
        while (true) {
          yield* Queue.take(arrived)
          yield* Effect.sleep(BATCH)
          yield* flush
          const now = yield* Clock.currentTimeMillis
          if (sent.filter((time) => now - time < NOISE_WINDOW).length > NOISE_LIMIT) return { kind: "noisy" as const }
        }
      })

      const end = yield* Effect.raceAll([
        handle.exitCode.pipe(
          Effect.map((code) => ({ kind: "exit" as const, code: Number(code) })),
          Effect.catch(() => Effect.succeed({ kind: "exit" as const, code: -1 })),
        ),
        Effect.sleep(input.timeout).pipe(Effect.as({ kind: "timeout" as const })),
        noisy,
      ])

      if (end.kind !== "exit") yield* handle.kill({ forceKillAfter: "3 seconds" }).pipe(Effect.ignore)
      // Let the last lines through. A grandchild can hold the pipe open after
      // the command itself is gone, so do not wait for it forever.
      yield* Fiber.join(reader).pipe(Effect.timeoutOption("1 second"))
      yield* flush

      const name = `Monitor "${input.description}"`
      const events = `${sent.length} ${sent.length === 1 ? "event" : "events"}`
      const final: Notification =
        end.kind === "exit"
          ? {
              id: task.id,
              outputFile: task.outputFile,
              status: end.code === 0 ? "completed" : "failed",
              summary: `${name} ended: the command exited with code ${end.code} after ${events}.`,
            }
          : end.kind === "timeout"
            ? {
                id: task.id,
                outputFile: task.outputFile,
                status: "killed",
                summary: `${name} timed out after ${Math.round(Duration.toMillis(input.timeout) / 1000)}s with ${events} and was stopped. Re-arm it if you still need the watch.`,
              }
            : {
                id: task.id,
                outputFile: task.outputFile,
                status: "killed",
                summary: `${name} was stopped: it produced too many events (more than ${NOISE_LIMIT} in a minute). Restart it with a tighter filter.`,
              }
      yield* deliver(input.sessionID, final)
      return final.summary
    }, Effect.scoped)

    const monitor: Interface["monitor"] = Effect.fn("BackgroundTask.monitor")(function* (input) {
      const id = `mon_${crypto.randomUUID().replaceAll("-", "").slice(0, 8)}`
      const task: Task = {
        id,
        type: "monitor",
        description: input.description,
        command: input.command,
        // Beside truncated tool output, which the read tool may open.
        outputFile: path.join(TRUNCATION_DIR, `${id}.log`),
      }
      yield* fs.ensureDir(TRUNCATION_DIR).pipe(Effect.orDie)
      yield* fs.writeFileString(task.outputFile, "").pipe(Effect.orDie)
      yield* jobs.start({
        id,
        type: task.type,
        title: input.description,
        // `ownerSessionId`, not `sessionId`: cancelling a session's run (Esc)
        // cancels jobs filed under `sessionId`, and a monitor must outlive it.
        metadata: { ownerSessionId: input.sessionID, command: input.command, outputFile: task.outputFile },
        run: watch(input, task),
      })
      return task
    })

    const owned = Effect.fnUntraced(function* (sessionID: SessionID) {
      return (yield* jobs.list()).filter(
        (job) => job.status === "running" && job.metadata?.ownerSessionId === sessionID,
      )
    })

    const stop: Interface["stop"] = Effect.fn("BackgroundTask.stop")(function* (sessionID, id) {
      const job = (yield* jobs.list()).find(
        (item) =>
          item.id === id &&
          item.status === "running" &&
          // Background subagents are filed under their parent session.
          (item.metadata?.ownerSessionId === sessionID || item.metadata?.parentSessionId === sessionID),
      )
      if (!job) return
      return yield* jobs.cancel(job.id)
    })

    const list: Interface["list"] = Effect.fn("BackgroundTask.list")(function* (sessionID) {
      return (yield* owned(sessionID)).map((job) => ({
        id: job.id,
        type: "monitor" as const,
        description: job.title ?? "",
        command: String(job.metadata?.command ?? ""),
        outputFile: String(job.metadata?.outputFile ?? ""),
      }))
    })

    return Service.of({ monitor, stop, list })
  }),
)

export const node = LayerNode.make({
  service: Service,
  layer,
  deps: [BackgroundJob.node, SessionWake.node, CrossSpawnSpawner.node, FSUtil.node],
})

export * as BackgroundTask from "./background"
