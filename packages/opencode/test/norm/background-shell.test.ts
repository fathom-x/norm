import { afterEach, beforeEach, describe, expect } from "bun:test"
import fs from "fs/promises"
import path from "path"
import { SessionV1 } from "@opencode-ai/core/v1/session"
import { CrossSpawnSpawner } from "@opencode-ai/core/cross-spawn-spawner"
import { Database } from "@opencode-ai/core/database/database"
import { LayerNode } from "@opencode-ai/core/effect/layer-node"
import { FSUtil } from "@opencode-ai/core/fs-util"
import { SessionProjector } from "@opencode-ai/core/session/projector"
import { Cause, Effect, Exit } from "effect"
import { Agent } from "@/agent/agent"
import { BackgroundJob } from "@/background/job"
import { Config } from "@/config/config"
import { RuntimeFlags } from "@/effect/runtime-flags"
import { EventV2Bridge } from "@/event-v2-bridge"
import { BackgroundTask } from "@/norm/background"
import { TaskStopTool } from "@/norm/tool/task-stop"
import { SessionWake } from "@/norm/wake"
import { Plugin } from "@/plugin"
import { SessionRunState } from "@/session/run-state"
import { MessageID, SessionID } from "@/session/schema"
import { Session } from "@/session/session"
import { SessionStatus } from "@/session/status"
import { ShellTool } from "@/tool/shell"
import * as Tool from "@/tool/tool"
import { Truncate } from "@/tool/truncate"
import { disposeAllInstances, TestInstance } from "../fixture/fixture"
import { testEffect } from "../lib/effect"

// The test preload turns the norm layer off (NORM_DISABLE=1), and background
// commands with it. Opt back in for these alone.
beforeEach(() => {
  process.env.NORM_DISABLE_WAKE = "0"
})

afterEach(async () => {
  delete process.env.NORM_DISABLE_WAKE
  await disposeAllInstances()
})

const shellNodes = [
  Agent.node,
  Config.node,
  CrossSpawnSpawner.node,
  FSUtil.node,
  Plugin.node,
  Truncate.node,
  RuntimeFlags.node,
]
const it = testEffect(
  LayerNode.compile(
    LayerNode.group([
      ...shellNodes,
      BackgroundJob.node,
      BackgroundTask.node,
      EventV2Bridge.node,
      Session.node,
      SessionProjector.node,
      SessionRunState.node,
      SessionStatus.node,
      SessionWake.node,
      Database.node,
    ]),
  ),
)
// What most of upstream's tests build: the shell tool without norm's engine.
const bare = testEffect(LayerNode.compile(LayerNode.group(shellNodes)))
const unix = process.platform === "win32" ? it.instance.skip : it.instance

const context = (sessionID: SessionID): Tool.Context => ({
  sessionID,
  messageID: MessageID.ascending(),
  agent: "build",
  abort: new AbortController().signal,
  messages: [],
  metadata: () => Effect.void,
  ask: () => Effect.void,
})

// The real shell tool and real processes; only SessionPrompt is replaced, by
// a recorder of what gets delivered to the session.
const setup = Effect.fn("BackgroundShellTest.setup")(function* () {
  const sessions = yield* Session.Service
  const wake = yield* SessionWake.Service
  const chat = yield* sessions.create({ title: "background" })
  const sent: string[] = []
  const reply = { info: { id: MessageID.ascending() }, parts: [] } as unknown as SessionV1.WithParts
  yield* wake.attach({
    prompt: (input) =>
      Effect.sync(() => {
        sent.push(input.parts.flatMap((part) => (part.type === "text" ? [part.text] : [])).join(""))
        return reply
      }),
    loop: () => Effect.succeed(reply),
  })
  const shell = yield* Tool.init(yield* ShellTool)
  const stop = yield* Tool.init(yield* TaskStopTool)
  return {
    chat,
    sent,
    shell,
    run: (command: string, options?: { timeout?: number; sessionID?: SessionID }) =>
      shell.execute(
        { command, run_in_background: true, timeout: options?.timeout },
        context(options?.sessionID ?? chat.id),
      ),
    stop: (id: string) => stop.execute({ task_id: id }, context(chat.id)),
  }
})

const eventually = Effect.fnUntraced(function* (check: () => boolean, timeout = 10_000) {
  const deadline = Date.now() + timeout
  while (!check()) {
    if (Date.now() > deadline) throw new Error("condition not met in time")
    yield* Effect.sleep("20 millis")
  }
})

function alive(pid: number) {
  try {
    process.kill(pid, 0)
    return true
  } catch {
    return false
  }
}

function started(output: string) {
  const match = output.match(
    /Command running in background with ID: (sh_[0-9a-f]{8})\. Output is being written to: (\S+)/,
  )
  if (!match) throw new Error(`not a background start: ${output}`)
  return { id: match[1], file: match[2] }
}

describe("bash run_in_background", () => {
  unix("returns at once and notifies the session once when the command exits", () =>
    Effect.gen(function* () {
      const test = yield* setup()
      const result = yield* test.run("sleep 1.5; echo built; echo warned >&2")
      const task = started(result.output)

      // Back before the command has finished.
      expect(yield* Effect.promise(() => fs.readFile(task.file, "utf8"))).toBe("")
      expect(result.metadata).toMatchObject({ exit: null, output: `Running in the background (task ${task.id})` })
      expect(test.sent).toEqual([])
      const listed = yield* (yield* BackgroundTask.Service).list(test.chat.id)
      expect(listed.map((item) => [item.id, item.type])).toEqual([[task.id, "shell"]])
      // Started without a timeout: nothing says when it will end.
      expect(listed[0].deadline).toBeUndefined()

      yield* eventually(() => test.sent.length === 1)
      expect(test.sent[0]).toBe(
        BackgroundTask.notification({
          id: task.id,
          outputFile: task.file,
          status: "completed",
          summary: 'Background command "sleep 1.5; echo built; echo warned >&2" completed (exit code 0).',
        }),
      )
      const file = yield* Effect.promise(() => fs.readFile(task.file, "utf8"))
      expect(file.split("\n").filter(Boolean).toSorted()).toEqual(["built", "warned"])
      yield* Effect.sleep("200 millis")
      expect(test.sent).toHaveLength(1)
    }),
  )

  unix("reports a failing command with its exit code", () =>
    Effect.gen(function* () {
      const test = yield* setup()
      const task = started((yield* test.run("echo nope; exit 7")).output)
      yield* eventually(() => test.sent.length === 1)

      expect(test.sent[0]).toContain("<status>failed</status>")
      expect(test.sent[0]).toContain('Background command "echo nope; exit 7" failed with exit code 7.')
      expect(test.sent[0]).toContain(`<task-id>${task.id}</task-id>`)
    }),
  )

  unix("an explicit timeout kills it, with a notice", () =>
    Effect.gen(function* () {
      const test = yield* setup()
      const dir = (yield* TestInstance).directory
      const before = Date.now()
      yield* test.run(`echo $$ > ${path.join(dir, "pid")}; while true; do sleep 0.2; done`, { timeout: 800 })
      const deadline = (yield* (yield* BackgroundTask.Service).list(test.chat.id))[0].deadline
      expect(deadline).toBeGreaterThanOrEqual(before + 800)
      expect(deadline).toBeLessThanOrEqual(Date.now() + 800)
      yield* eventually(() => test.sent.length === 1)

      expect(test.sent[0]).toContain("<status>killed</status>")
      expect(test.sent[0]).toContain("was killed after exceeding its timeout of 800ms.")
      const pid = Number(yield* Effect.promise(() => fs.readFile(path.join(dir, "pid"), "utf8")))
      yield* eventually(() => !alive(pid))
    }),
  )

  unix("TaskStop stops it without a notification", () =>
    Effect.gen(function* () {
      const test = yield* setup()
      const dir = (yield* TestInstance).directory
      const task = started(
        (yield* test.run(`echo $$ > ${path.join(dir, "pid")}; while true; do sleep 0.2; done`)).output,
      )
      yield* eventually(() => Bun.file(path.join(dir, "pid")).size > 0)
      const pid = Number(yield* Effect.promise(() => fs.readFile(path.join(dir, "pid"), "utf8")))

      expect((yield* test.stop(task.id)).output).toContain(`Successfully stopped task: ${task.id}`)
      yield* eventually(() => !alive(pid))
      yield* Effect.sleep("300 millis")
      expect(test.sent).toEqual([])
    }),
  )

  unix("is refused in a subagent session", () =>
    Effect.gen(function* () {
      const test = yield* setup()
      const child = yield* (yield* Session.Service).create({ parentID: test.chat.id, title: "child" })
      const exit = yield* test.run("echo hi", { sessionID: child.id }).pipe(Effect.exit)

      expect(Exit.isFailure(exit) ? Cause.pretty(exit.cause) : "").toContain(
        "run_in_background is not available to subagents",
      )
      expect(test.sent).toEqual([])
    }),
  )

  unix("is advertised to the model only when it works", () =>
    Effect.gen(function* () {
      const on = yield* Tool.init(yield* ShellTool)
      expect(on.jsonSchema).toBeUndefined()
      expect(Object.keys(on.parameters.fields)).toContain("run_in_background")
      expect(on.description).toContain("# Background commands")

      process.env.NORM_DISABLE_WAKE = "1"
      const off = yield* Tool.init(yield* ShellTool)
      expect(Object.keys((off.jsonSchema as { properties: object }).properties).toSorted()).toEqual([
        "command",
        "timeout",
        "workdir",
      ])
      expect(off.description).not.toContain("run_in_background")
    }),
  )

  bare.instance("runs in the foreground when the engine is not there", () =>
    Effect.gen(function* () {
      const shell = yield* Tool.init(yield* ShellTool)
      expect(shell.description).not.toContain("run_in_background")
      const result = yield* shell.execute(
        { command: "echo foreground", run_in_background: true },
        context(SessionID.make("ses_bare")),
      )
      expect(result.output).toContain("foreground")
      expect(result.metadata.exit).toBe(0)
    }),
  )
})
