import { createWriteStream } from "node:fs"
import path from "path"
import { LayerNode } from "@opencode-ai/core/effect/layer-node"
import { CrossSpawnSpawner } from "@opencode-ai/core/cross-spawn-spawner"
import { FSUtil } from "@opencode-ai/core/fs-util"
import { Cause, Clock, Context, Duration, Effect, Fiber, Layer, Queue, Schema, Stream } from "effect"
import { ChildProcess } from "effect/unstable/process"
import { ChildProcessSpawner } from "effect/unstable/process/ChildProcessSpawner"
import { BackgroundJob } from "@/background/job"
import { SessionID } from "@/session/schema"
import { Session } from "@/session/session"
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
  type: "monitor" | "shell"
  description: string
  command: string
  outputFile: string
  /** When it is killed if still running. A command started without a timeout has none. */
  deadline?: number
}

export type MonitorInput = Launch & {
  sessionID: SessionID
  description: string
  timeout: Duration.Duration
}

export type ShellInput = Launch & {
  sessionID: SessionID
  /** Killed after this long, with a notice. Runs until it exits when left out. */
  timeout?: Duration.Duration
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
  /**
   * Runs the command outside any turn, all output going to a file, and
   * notifies the session once when it exits.
   */
  readonly shell: (input: ShellInput) => Effect.Effect<Task, SubagentError>
  /** Stops a task the session started. Returns it, or nothing if it has none running by that id. */
  readonly stop: (sessionID: SessionID, id: string) => Effect.Effect<BackgroundJob.Info | undefined>
  /** The session's running tasks. */
  readonly list: (sessionID: SessionID) => Effect.Effect<Task[]>
}

export class Service extends Context.Service<Service, Interface>()("@opencode/BackgroundTask") {}

/**
 * A subagent runs inside its parent's turn. A command it left running would
 * report back to the subagent's session, starting a turn nobody reads.
 */
export class SubagentError extends Schema.TaggedErrorClass<SubagentError>()("BackgroundTaskSubagentError", {}) {
  override get message() {
    return "run_in_background is not available to subagents. Run the command in the foreground instead."
  }
}

/** What the bash tool's schema gains when background commands are on. */
export const ShellFields = {
  run_in_background: Schema.optional(Schema.Boolean).annotate({
    description:
      "Set to true to run this command in the background. It keeps running across turns and you are notified when it exits.",
  }),
}

/** Appended to the bash tool's description when background commands are on. */
export const SHELL_NOTE = `

# Background commands
- \`run_in_background\` runs the command detached: it keeps running across turns and re-invokes you when it exits. No \`&\` needed.
- Use it for long-running commands (dev servers, builds, test suites), and to wait for a condition with a command that exits when it is true, e.g. \`until grep -q "Ready in" dev.log; do sleep 0.5; done\`. You get a single completion notification when it exits.
- The call returns at once with a task id and an output file. All output goes to that file; read it with the read tool. Stop the command with TaskStop.
- Do not sleep or poll while it runs. \`timeout\`, when given, kills it after that long.
- To be notified of every matching line rather than once at exit, use the Monitor tool instead.`

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
    const sessions = yield* Session.Service

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

    const start = Effect.fnUntraced(function* (
      sessionID: SessionID,
      input: Pick<Task, "type" | "description" | "command">,
      timeout: Duration.Duration | undefined,
      run: (task: Task) => Effect.Effect<string>,
    ) {
      const id = `${input.type === "monitor" ? "mon" : "sh"}_${crypto.randomUUID().replaceAll("-", "").slice(0, 8)}`
      // Beside truncated tool output, which the read tool may open.
      const task: Task = {
        ...input,
        id,
        outputFile: path.join(TRUNCATION_DIR, `${id}.log`),
        deadline: timeout ? (yield* Clock.currentTimeMillis) + Duration.toMillis(timeout) : undefined,
      }
      yield* fs.ensureDir(TRUNCATION_DIR).pipe(Effect.orDie)
      yield* fs.writeFileString(task.outputFile, "").pipe(Effect.orDie)
      yield* jobs.start({
        id,
        type: task.type,
        title: input.description,
        // `ownerSessionId`, not `sessionId`: cancelling a session's run (Esc)
        // cancels jobs filed under `sessionId`, and these must outlive it.
        metadata: {
          ownerSessionId: sessionID,
          command: input.command,
          outputFile: task.outputFile,
          deadline: task.deadline,
        },
        run: run(task),
      })
      return task
    })

    const monitor: Interface["monitor"] = Effect.fn("BackgroundTask.monitor")(function* (input) {
      return yield* start(
        input.sessionID,
        { type: "monitor", description: input.description, command: input.command },
        input.timeout,
        (task) => watch(input, task),
      )
    })

    const wait = Effect.fn("BackgroundTask.wait")(function* (input: ShellInput, task: Task) {
      const handle = yield* spawner.spawn(input.process).pipe(Effect.orDie)
      yield* Effect.addFinalizer(() => handle.kill({ forceKillAfter: "3 seconds" }).pipe(Effect.ignore))
      const sink = yield* Effect.acquireRelease(
        Effect.sync(() => createWriteStream(task.outputFile, { flags: "a" })),
        (stream) => Effect.promise(() => new Promise<void>((resolve) => stream.end(resolve))),
      )
      const reader = yield* Stream.runForEach(Stream.decodeText(handle.all), (chunk) =>
        Effect.sync(() => sink.write(chunk)),
      ).pipe(Effect.ignore, Effect.forkScoped)

      const end = yield* Effect.raceAll([
        handle.exitCode.pipe(
          Effect.map((code) => ({ kind: "exit" as const, code: Number(code) })),
          Effect.catch(() => Effect.succeed({ kind: "exit" as const, code: -1 })),
        ),
        ...(input.timeout ? [Effect.sleep(input.timeout).pipe(Effect.as({ kind: "timeout" as const }))] : []),
      ])
      if (end.kind === "timeout") yield* handle.kill({ forceKillAfter: "3 seconds" }).pipe(Effect.ignore)
      yield* Fiber.join(reader).pipe(Effect.timeoutOption("1 second"))

      const name = `Background command "${task.description}"`
      const final: Notification =
        end.kind === "timeout"
          ? {
              id: task.id,
              outputFile: task.outputFile,
              status: "killed",
              summary: `${name} was killed after exceeding its timeout of ${Duration.toMillis(input.timeout ?? Duration.zero)}ms.`,
            }
          : {
              id: task.id,
              outputFile: task.outputFile,
              status: end.code === 0 ? "completed" : "failed",
              summary:
                end.code === 0 ? `${name} completed (exit code 0).` : `${name} failed with exit code ${end.code}.`,
            }
      yield* deliver(input.sessionID, final)
      return final.summary
    }, Effect.scoped)

    const shell: Interface["shell"] = Effect.fn("BackgroundTask.shell")(function* (input) {
      const session = yield* sessions.get(input.sessionID).pipe(Effect.orDie)
      if (session.parentID) return yield* new SubagentError()
      return yield* start(
        input.sessionID,
        { type: "shell", description: summarize(input.command), command: input.command },
        input.timeout,
        (task) => wait(input, task),
      )
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
        type: job.type === "monitor" ? ("monitor" as const) : ("shell" as const),
        description: job.title ?? "",
        command: String(job.metadata?.command ?? ""),
        outputFile: String(job.metadata?.outputFile ?? ""),
        deadline: typeof job.metadata?.deadline === "number" ? job.metadata.deadline : undefined,
      }))
    })

    return Service.of({ monitor, shell, stop, list })
  }),
)

export const node = LayerNode.make({
  service: Service,
  layer,
  deps: [BackgroundJob.node, SessionWake.node, CrossSpawnSpawner.node, FSUtil.node, Session.node],
})

// A command can be a whole script; notifications name it by its first line.
function summarize(command: string) {
  const line = command.trim().split("\n")[0]
  return line.length > 80 ? `${line.slice(0, 77)}...` : line
}

export * as BackgroundTask from "./background"
