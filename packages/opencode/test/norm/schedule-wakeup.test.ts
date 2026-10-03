import { afterEach, beforeEach, describe, expect } from "bun:test"
import { SessionV1 } from "@opencode-ai/core/v1/session"
import { Database } from "@opencode-ai/core/database/database"
import { LayerNode } from "@opencode-ai/core/effect/layer-node"
import { SessionProjector } from "@opencode-ai/core/session/projector"
import { Cause, Clock, Effect, Exit } from "effect"
import * as TestClock from "effect/testing/TestClock"
import { Agent } from "@/agent/agent"
import { BackgroundJob } from "@/background/job"
import { Command } from "@/command"
import { Config } from "@/config/config"
import { RuntimeFlags } from "@/effect/runtime-flags"
import { EventV2Bridge } from "@/event-v2-bridge"
import { BackgroundTask } from "@/norm/background"
import { SessionCron } from "@/norm/cron"
import { NormTools } from "@/norm/tools"
import { Plugin } from "@/plugin"
import { Parameters, ScheduleWakeupTool } from "@/norm/tool/schedule-wakeup"
import { SessionWake } from "@/norm/wake"
import { Permission } from "@/permission"
import type { SessionPrompt } from "@/session/prompt"
import { SessionRunState } from "@/session/run-state"
import { MessageID } from "@/session/schema"
import { Session } from "@/session/session"
import { SessionStatus } from "@/session/status"
import { ToolJsonSchema } from "@/tool/json-schema"
import { ToolRegistry } from "@/tool/registry"
import { Truncate } from "@/tool/truncate"
import { CrossSpawnSpawner } from "@opencode-ai/core/cross-spawn-spawner"
import { FSUtil } from "@opencode-ai/core/fs-util"
import { ModelV2 } from "@opencode-ai/core/model"
import { ProviderV2 } from "@opencode-ai/core/provider"
import { Ripgrep } from "@opencode-ai/core/ripgrep"
import { disposeAllInstances, withTmpdirInstance } from "../fixture/fixture"
import { testEffect } from "../lib/effect"

// The test preload runs with the norm layer off (NORM_DISABLE=1), which takes
// these tools with it. Opt back in for the tools alone.
beforeEach(() => {
  process.env.NORM_DISABLE_WAKE = "0"
})

afterEach(async () => {
  delete process.env.NORM_DISABLE_WAKE
  await disposeAllInstances()
})

const it = testEffect(
  LayerNode.compile(
    LayerNode.group([
      ToolRegistry.node,
      CrossSpawnSpawner.node,
      Ripgrep.node,
      FSUtil.node,
      Plugin.node,
      BackgroundTask.node,
      SessionCron.node,
      Agent.node,
      BackgroundJob.node,
      Command.node,
      Config.node,
      EventV2Bridge.node,
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

// The tool with SessionPrompt replaced by a recorder, so a test sees exactly
// what a fired wakeup sends.
const setup = Effect.fn("ScheduleWakeupTest.setup")(function* () {
  const sessions = yield* Session.Service
  const wake = yield* SessionWake.Service
  const chat = yield* sessions.create({ title: "loop" })
  const sent: SessionPrompt.PromptInput[] = []
  const reply = { info: { id: MessageID.ascending() }, parts: [] } as unknown as SessionV1.WithParts
  yield* wake.attach({
    prompt: (input) =>
      Effect.sync(() => {
        sent.push(input)
        return reply
      }),
    loop: () => Effect.succeed(reply),
  })
  const def = yield* (yield* ScheduleWakeupTool).init()
  return {
    chat,
    wake,
    sent,
    call: (params: typeof Parameters.Type) =>
      def.execute(params, {
        sessionID: chat.id,
        messageID: MessageID.ascending(),
        agent: "build",
        abort: new AbortController().signal,
        messages: [],
        metadata: () => Effect.void,
        ask: () => Effect.void,
      }),
  }
})

const loop = { prompt: "check the deploy", reason: "deploy takes about five minutes", noop: true }

describe("ScheduleWakeup", () => {
  it.live("has Claude Code's name and parameters", () =>
    Effect.gen(function* () {
      expect(ScheduleWakeupTool.id).toBe("ScheduleWakeup")
      const schema = ToolJsonSchema.fromSchema(Parameters) as {
        properties: Record<string, unknown>
        required?: string[]
      }
      expect(Object.keys(schema.properties).toSorted()).toEqual(["delaySeconds", "noop", "prompt", "reason", "stop"])
      // `stop: true` is sent alone, so nothing can be required by the schema.
      expect(schema.required ?? []).toEqual([])
    }),
  )

  it.effect("fires the prompt into the session after the delay", () =>
    Effect.gen(function* () {
      const test = yield* setup()
      const result = yield* test.call({ ...loop, delaySeconds: 300 })

      expect(result.title).toBe("Wake in 5m: deploy takes about five minutes")
      expect(result.output).toContain("Wakeup scheduled in 300s.")
      expect(result.metadata).toMatchObject({ stopped: false, delaySeconds: 300, noop: true })
      expect(yield* test.wake.pending(test.chat.id)).toEqual([{ key: "wakeup", at: result.metadata.at! }])

      yield* TestClock.adjust("299 seconds")
      expect(test.sent).toEqual([])

      yield* TestClock.adjust("1 second")
      expect(test.sent).toHaveLength(1)
      expect(test.sent[0]).toMatchObject({ sessionID: test.chat.id, noReply: true })
      // The prompt is shown like the user's own; the note is for the model only.
      expect(test.sent[0].parts).toMatchObject([
        { type: "text", text: "check the deploy", synthetic: false },
        { type: "text", synthetic: true },
      ])
      expect(test.sent[0].parts[1]).toMatchObject({
        text: expect.stringContaining("ScheduleWakeup (deploy takes about five minutes), not typed by the user"),
      })
      expect(yield* test.wake.pending(test.chat.id)).toEqual([])
    }).pipe(withTmpdirInstance()),
  )

  it.effect("clamps the delay to between one minute and one hour", () =>
    Effect.gen(function* () {
      const test = yield* setup()
      const now = yield* Clock.currentTimeMillis

      const short = yield* test.call({ ...loop, delaySeconds: 5 })
      expect(short.metadata.delaySeconds).toBe(60)
      expect(short.output).toContain("Wakeup scheduled in 60s (clamped from 5s).")
      expect(short.metadata.at).toBe(now + 60_000)

      const long = yield* test.call({ ...loop, delaySeconds: 86_400 })
      expect(long.metadata.delaySeconds).toBe(3600)
      expect(long.title).toBe("Wake in 60m: deploy takes about five minutes")
      expect(long.metadata.at).toBe(now + 3_600_000)
    }).pipe(withTmpdirInstance()),
  )

  it.effect("scheduling again replaces the pending wakeup", () =>
    Effect.gen(function* () {
      const test = yield* setup()
      yield* test.call({ ...loop, delaySeconds: 60, prompt: "first" })
      yield* test.call({ ...loop, delaySeconds: 120, prompt: "second" })
      expect(yield* test.wake.pending(test.chat.id)).toHaveLength(1)

      yield* TestClock.adjust("1 hour")
      expect(test.sent.map((input) => input.parts[0])).toMatchObject([{ text: "second" }])
    }).pipe(withTmpdirInstance()),
  )

  it.effect("stop cancels the pending wakeup and ignores the other fields", () =>
    Effect.gen(function* () {
      const test = yield* setup()
      yield* test.call({ ...loop, delaySeconds: 60 })

      const stopped = yield* test.call({ stop: true, delaySeconds: 60, prompt: "ignored" })
      expect(stopped.metadata).toMatchObject({ stopped: true })
      expect(stopped.output).toContain("The pending wakeup was cancelled")
      expect(yield* test.wake.pending(test.chat.id)).toEqual([])

      expect((yield* test.call({ stop: true })).output).toContain("No wakeup was pending")
      yield* TestClock.adjust("1 hour")
      expect(test.sent).toEqual([])
    }).pipe(withTmpdirInstance()),
  )

  it.effect("the user stopping the session clears the wakeup", () =>
    Effect.gen(function* () {
      const test = yield* setup()
      yield* test.call({ ...loop, delaySeconds: 60 })
      yield* test.wake.clear(test.chat.id)

      yield* TestClock.adjust("1 hour")
      expect(test.sent).toEqual([])
    }).pipe(withTmpdirInstance()),
  )

  it.effect("rejects a schedule that is missing a field", () =>
    Effect.gen(function* () {
      const test = yield* setup()
      const error = (params: typeof Parameters.Type) =>
        test.call(params).pipe(
          Effect.exit,
          Effect.map((exit) => (Exit.isFailure(exit) ? Cause.pretty(exit.cause) : "")),
        )

      expect(yield* error({ prompt: "p", reason: "r" })).toContain("delaySeconds is required unless stop is true")
      expect(yield* error({ delaySeconds: 60, reason: "r", prompt: " " })).toContain("prompt is required")
      expect(yield* error({ delaySeconds: 60, prompt: "p" })).toContain("reason is required")
      expect(yield* test.wake.pending(test.chat.id)).toEqual([])
    }).pipe(withTmpdirInstance()),
  )

  it.live("is kept from subagents and can be switched off", () =>
    Effect.gen(function* () {
      expect(NormTools.primaryOnly()).toContain("ScheduleWakeup")
      const denied = Permission.disabled(
        ["ScheduleWakeup", "bash"],
        NormTools.primaryOnly().map((permission) => ({ permission, pattern: "*", action: "deny" as const })),
      )
      expect(denied.has("ScheduleWakeup")).toBe(true)
      expect(denied.has("bash")).toBe(false)

      expect((yield* NormTools.infos).map((tool) => tool.id)).toContain("ScheduleWakeup")
      process.env.NORM_DISABLE_WAKE = "1"
      expect(yield* NormTools.infos).toEqual([])
      expect(NormTools.primaryOnly()).toEqual([])

      // Unset, they follow NORM_DISABLE.
      delete process.env.NORM_DISABLE_WAKE
      const layer = process.env.NORM_DISABLE
      process.env.NORM_DISABLE = "1"
      expect(NormTools.disabled()).toBe(true)
      delete process.env.NORM_DISABLE
      expect(NormTools.disabled()).toBe(false)
      if (layer !== undefined) process.env.NORM_DISABLE = layer
    }),
  )

  it.instance("is offered to the model beside the built-in tools", () =>
    Effect.gen(function* () {
      const tools = yield* (yield* ToolRegistry.Service).tools({
        providerID: ProviderV2.ID.opencode,
        modelID: ModelV2.ID.make("claude-sonnet-5"),
        agent: yield* (yield* Agent.Service).defaultInfo(),
      })
      const tool = tools.find((item) => item.id === "ScheduleWakeup")
      expect(tool?.description).toContain("Schedule when to resume work in /loop dynamic mode")
      expect(tools.map((item) => item.id)).toContain("bash")
    }),
  )

  it.instance("/loop is a built-in command that hands the task to the tool", () =>
    Effect.gen(function* () {
      const loop = yield* (yield* Command.Service).get("loop")
      expect(loop?.hints).toEqual(["$ARGUMENTS"])
      expect(yield* Effect.promise(async () => loop?.template)).toContain("ScheduleWakeup")
    }),
  )
})
