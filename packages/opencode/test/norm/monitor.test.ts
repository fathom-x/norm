import { afterEach, describe, expect } from "bun:test"
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
import { MonitorTool, Parameters } from "@/norm/tool/monitor"
import { TaskStopTool } from "@/norm/tool/task-stop"
import { SessionWake } from "@/norm/wake"
import { Plugin } from "@/plugin"
import { SessionRunState } from "@/session/run-state"
import { MessageID, SessionID } from "@/session/schema"
import { Session } from "@/session/session"
import { SessionStatus } from "@/session/status"
import { ToolJsonSchema } from "@/tool/json-schema"
import * as Tool from "@/tool/tool"
import { Truncate } from "@/tool/truncate"
import { disposeAllInstances, disposeAllInstancesEffect, TestInstance } from "../fixture/fixture"
import { testEffect } from "../lib/effect"

afterEach(async () => {
  await disposeAllInstances()
})

const it = testEffect(
  LayerNode.compile(
    LayerNode.group([
      Agent.node,
      BackgroundJob.node,
      BackgroundTask.node,
      Config.node,
      CrossSpawnSpawner.node,
      EventV2Bridge.node,
      FSUtil.node,
      Plugin.node,
      Session.node,
      SessionProjector.node,
      SessionRunState.node,
      SessionStatus.node,
      SessionWake.node,
      Truncate.node,
      Database.node,
      RuntimeFlags.node,
    ]),
  ),
)
const unix = process.platform === "win32" ? it.instance.skip : it.instance

// The real tools and real processes; only SessionPrompt is replaced, by a
// recorder of what gets delivered to the session.
const setup = Effect.fn("MonitorTest.setup")(function* () {
  const sessions = yield* Session.Service
  const wake = yield* SessionWake.Service
  const chat = yield* sessions.create({ title: "monitor" })
  const sent: string[] = []
  const asked: Array<{ permission: string; patterns: readonly string[] }> = []
  const reply = { info: { id: MessageID.ascending() }, parts: [] } as unknown as SessionV1.WithParts
  yield* wake.attach({
    prompt: (input) =>
      Effect.sync(() => {
        sent.push(input.parts.flatMap((part) => (part.type === "text" ? [part.text] : [])).join(""))
        return reply
      }),
    loop: () => Effect.succeed(reply),
  })
  const ctx = (sessionID: SessionID): Tool.Context => ({
    sessionID,
    messageID: MessageID.ascending(),
    agent: "build",
    abort: new AbortController().signal,
    messages: [],
    metadata: () => Effect.void,
    ask: (request) =>
      Effect.sync(() => {
        asked.push({ permission: request.permission, patterns: request.patterns })
      }),
  })
  const monitor = yield* Tool.init(yield* MonitorTool)
  const stop = yield* Tool.init(yield* TaskStopTool)
  return {
    chat,
    sent,
    asked,
    monitor: (params: typeof Parameters.Type, sessionID = chat.id) => monitor.execute(params, ctx(sessionID)),
    stop: (id: string, sessionID = chat.id) => stop.execute({ task_id: id }, ctx(sessionID)),
  }
})

const eventually = Effect.fnUntraced(function* (check: () => boolean | Promise<boolean>, timeout = 10_000) {
  const deadline = Date.now() + timeout
  while (!(yield* Effect.promise(async () => check()))) {
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

const failure = <A, E>(effect: Effect.Effect<A, E>) =>
  effect.pipe(
    Effect.exit,
    Effect.map((exit) => (Exit.isFailure(exit) ? Cause.pretty(exit.cause) : "")),
  )

describe("Monitor", () => {
  it.live("has Claude Code's names and parameters", () =>
    Effect.gen(function* () {
      expect(MonitorTool.id).toBe("Monitor")
      expect(TaskStopTool.id).toBe("TaskStop")
      const schema = ToolJsonSchema.fromSchema(Parameters) as { properties: Record<string, unknown> }
      expect(Object.keys(schema.properties).toSorted()).toEqual(["command", "description", "timeout_ms"])
    }),
  )

  it.live("renders notifications as task-notification blocks", () =>
    Effect.gen(function* () {
      expect(
        BackgroundTask.notification({ id: "mon_1", summary: 'Monitor event: "errors"', event: "boom\nagain" }),
      ).toBe(
        [
          "<task-notification>",
          "<task-id>mon_1</task-id>",
          '<summary>Monitor event: "errors"</summary>',
          "<event>boom\nagain</event>",
          "</task-notification>",
        ].join("\n"),
      )
      expect(
        BackgroundTask.notification({ id: "mon_1", summary: "done", status: "completed", outputFile: "/tmp/out" }),
      ).toBe(
        [
          "<task-notification>",
          "<task-id>mon_1</task-id>",
          "<output-file>/tmp/out</output-file>",
          "<status>completed</status>",
          "<summary>done</summary>",
          "</task-notification>",
        ].join("\n"),
      )
    }),
  )

  unix("delivers stdout lines as batched events, keeps stderr in the file, and reports the exit", () =>
    Effect.gen(function* () {
      const test = yield* setup()
      const result = yield* test.monitor({
        description: "build steps",
        command: "echo one; echo two; echo oops >&2; sleep 0.8; echo three",
      })
      const id = result.metadata.taskId

      expect(result.title).toBe("build steps")
      expect(result.output).toContain(`Monitor started with task id ${id}.`)
      expect(result.metadata.timeoutMs).toBe(300_000)
      // Asked under the bash tool's permission, with the command as the pattern.
      expect(test.asked).toHaveLength(1)
      expect(test.asked[0].permission).toBe("bash")
      expect(test.asked[0].patterns.join(" ")).toContain("echo one")

      yield* eventually(() => test.sent.length === 3)
      expect(test.sent[0]).toBe(
        BackgroundTask.notification({ id, summary: 'Monitor event: "build steps"', event: "one\ntwo" }),
      )
      expect(test.sent[1]).toBe(
        BackgroundTask.notification({ id, summary: 'Monitor event: "build steps"', event: "three" }),
      )
      expect(test.sent[2]).toBe(
        BackgroundTask.notification({
          id,
          outputFile: result.metadata.outputFile,
          status: "completed",
          summary: 'Monitor "build steps" ended: the command exited with code 0 after 2 events.',
        }),
      )
      const file = yield* Effect.promise(() => fs.readFile(result.metadata.outputFile, "utf8"))
      expect(file.split("\n").filter(Boolean).toSorted()).toEqual(["one", "oops", "three", "two"])
      expect(yield* (yield* BackgroundTask.Service).list(test.chat.id)).toEqual([])
    }),
  )

  unix("reports a failing command", () =>
    Effect.gen(function* () {
      const test = yield* setup()
      yield* test.monitor({ description: "flaky job", command: "echo started; exit 3" })
      yield* eventually(() => test.sent.length === 2)

      expect(test.sent[1]).toContain("<status>failed</status>")
      expect(test.sent[1]).toContain('Monitor "flaky job" ended: the command exited with code 3 after 1 event.')
    }),
  )

  unix("kills the command at the deadline and says how many events it saw", () =>
    Effect.gen(function* () {
      const test = yield* setup()
      const dir = (yield* TestInstance).directory
      const result = yield* test.monitor({
        description: "quiet log",
        command: `echo $$ > ${path.join(dir, "pid")}; echo armed; while true; do sleep 0.2; done`,
        timeout_ms: 1500,
      })
      expect(result.metadata.timeoutMs).toBe(1500)
      yield* eventually(() => test.sent.length === 2)

      expect(test.sent[0]).toContain("<event>armed</event>")
      expect(test.sent[1]).toContain("<status>killed</status>")
      expect(test.sent[1]).toContain(
        'Monitor "quiet log" timed out after 2s with 1 event and was stopped. Re-arm it if you still need the watch.',
      )
      const pid = Number(yield* Effect.promise(() => fs.readFile(path.join(dir, "pid"), "utf8")))
      yield* eventually(() => !alive(pid))
    }),
  )

  unix("clamps the deadline", () =>
    Effect.gen(function* () {
      const test = yield* setup()
      expect((yield* test.monitor({ description: "a", command: "true", timeout_ms: 5 })).metadata.timeoutMs).toBe(1000)
      expect((yield* test.monitor({ description: "b", command: "true", timeout_ms: 9e9 })).metadata.timeoutMs).toBe(
        1_800_000,
      )
      expect(yield* failure(test.monitor({ description: " ", command: "true" }))).toContain("description is required")
    }),
  )

  unix("TaskStop kills the whole process tree without a notification", () =>
    Effect.gen(function* () {
      const test = yield* setup()
      const dir = (yield* TestInstance).directory
      const result = yield* test.monitor({
        description: "server log",
        command: `sleep 100 & echo $! > ${path.join(dir, "child")}; echo $$ > ${path.join(dir, "pid")}; echo up; wait`,
      })
      yield* eventually(() => test.sent.length === 1)
      const read = (name: string) => Effect.promise(async () => Number(await fs.readFile(path.join(dir, name), "utf8")))
      const pids = [yield* read("pid"), yield* read("child")]
      expect(pids.every(alive)).toBe(true)
      const listed = yield* (yield* BackgroundTask.Service).list(test.chat.id)
      expect(listed.map((task) => task.id)).toEqual([result.metadata.taskId])
      // A monitor always has a deadline (here the default five minutes).
      expect(listed[0].deadline).toBeGreaterThan(Date.now() + 290_000)

      // Another session cannot stop it.
      const other = yield* (yield* Session.Service).create({ title: "other" })
      expect(yield* failure(test.stop(result.metadata.taskId, other.id))).toContain(
        `No running task found with ID: ${result.metadata.taskId}`,
      )

      const stopped = yield* test.stop(result.metadata.taskId)
      expect(stopped.output).toBe(`Successfully stopped task: ${result.metadata.taskId} (server log)`)
      yield* eventually(() => !pids.some(alive))
      yield* Effect.sleep("300 millis")
      expect(test.sent).toHaveLength(1)
      expect(yield* failure(test.stop(result.metadata.taskId))).toContain("No running task found")
    }),
  )

  unix("survives the user interrupting a turn, but not its session being deleted", () =>
    Effect.gen(function* () {
      const test = yield* setup()
      const dir = (yield* TestInstance).directory
      yield* test.monitor({
        description: "server log",
        command: `echo $$ > ${path.join(dir, "pid")}; echo up; while true; do sleep 0.2; done`,
      })
      yield* eventually(() => test.sent.length === 1)
      const pid = Number(yield* Effect.promise(() => fs.readFile(path.join(dir, "pid"), "utf8")))

      yield* (yield* SessionRunState.Service).cancel(test.chat.id)
      yield* Effect.sleep("300 millis")
      expect(alive(pid)).toBe(true)

      yield* (yield* Session.Service).remove(test.chat.id)
      yield* eventually(() => !alive(pid))
    }),
  )

  unix("dies with its instance", () =>
    Effect.gen(function* () {
      const test = yield* setup()
      const dir = (yield* TestInstance).directory
      yield* test.monitor({
        description: "server log",
        command: `echo $$ > ${path.join(dir, "pid")}; echo up; while true; do sleep 0.2; done`,
      })
      yield* eventually(() => test.sent.length === 1)
      const pid = Number(yield* Effect.promise(() => fs.readFile(path.join(dir, "pid"), "utf8")))
      expect(alive(pid)).toBe(true)

      // What a config reload or quitting the TUI does.
      yield* disposeAllInstancesEffect
      yield* eventually(() => !alive(pid))
    }),
  )

  unix(
    "stops a monitor that floods the conversation",
    () =>
      Effect.gen(function* () {
        const test = yield* setup()
        yield* test.monitor({
          description: "raw log",
          command: "i=0; while true; do i=$((i+1)); echo line $i; sleep 0.25; done",
        })
        yield* eventually(() => test.sent.some((text) => text.includes("<status>killed</status>")), 25_000)

        const last = test.sent.at(-1)!
        expect(last).toContain(
          'Monitor "raw log" was stopped: it produced too many events (more than 30 in a minute). Restart it with a tighter filter.',
        )
        expect(test.sent.filter((text) => text.includes("<event>")).length).toBe(31)
      }),
    30_000,
  )
})
