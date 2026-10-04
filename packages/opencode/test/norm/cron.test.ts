import { afterEach, describe, expect } from "bun:test"
import { SessionV1 } from "@opencode-ai/core/v1/session"
import { Database } from "@opencode-ai/core/database/database"
import { LayerNode } from "@opencode-ai/core/effect/layer-node"
import { SessionProjector } from "@opencode-ai/core/session/projector"
import { Cause, Deferred, Effect, Exit, Scope } from "effect"
import * as TestClock from "effect/testing/TestClock"
import { Agent } from "@/agent/agent"
import { BackgroundJob } from "@/background/job"
import { Config } from "@/config/config"
import { RuntimeFlags } from "@/effect/runtime-flags"
import { EventV2Bridge } from "@/event-v2-bridge"
import { SessionCron } from "@/norm/cron"
import { CreateParameters, CronCreateTool, CronDeleteTool, CronListTool } from "@/norm/tool/cron"
import { SessionWake } from "@/norm/wake"
import type { SessionPrompt } from "@/session/prompt"
import { SessionRunState } from "@/session/run-state"
import { MessageID } from "@/session/schema"
import { Session } from "@/session/session"
import { SessionStatus } from "@/session/status"
import { ToolJsonSchema } from "@/tool/json-schema"
import * as Tool from "@/tool/tool"
import { Truncate } from "@/tool/truncate"
import { disposeAllInstances, withTmpdirInstance } from "../fixture/fixture"
import { testEffect } from "../lib/effect"

afterEach(async () => {
  await disposeAllInstances()
})

const it = testEffect(
  LayerNode.compile(
    LayerNode.group([
      Agent.node,
      BackgroundJob.node,
      Config.node,
      EventV2Bridge.node,
      Session.node,
      SessionCron.node,
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

// The real tools on a test clock that starts at the epoch; SessionPrompt is
// replaced by a recorder of what each fire sends to the session.
const setup = Effect.fn("CronTest.setup")(function* () {
  const sessions = yield* Session.Service
  const wake = yield* SessionWake.Service
  const chat = yield* sessions.create({ title: "cron" })
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
  const ctx: Tool.Context = {
    sessionID: chat.id,
    messageID: MessageID.ascending(),
    agent: "build",
    abort: new AbortController().signal,
    messages: [],
    metadata: () => Effect.void,
    ask: () => Effect.void,
  }
  const create = yield* Tool.init(yield* CronCreateTool)
  const list = yield* Tool.init(yield* CronListTool)
  const remove = yield* Tool.init(yield* CronDeleteTool)
  return {
    chat,
    wake,
    sent,
    reply,
    texts: () => sent.map((input) => (input.parts[0].type === "text" ? input.parts[0].text : "")),
    create: (params: typeof CreateParameters.Type) => create.execute(params, ctx),
    list: () => list.execute({}, ctx),
    remove: (id: string) => remove.execute({ id }, ctx),
  }
})

const failure = <A, E>(effect: Effect.Effect<A, E>) =>
  effect.pipe(
    Effect.exit,
    Effect.map((exit) => (Exit.isFailure(exit) ? Cause.pretty(exit.cause) : "")),
  )

const minutes = (count: number) => count * 60_000

describe("cron tools", () => {
  it.live("have Claude Code's names and parameters", () =>
    Effect.gen(function* () {
      expect([CronCreateTool.id, CronListTool.id, CronDeleteTool.id]).toEqual(["CronCreate", "CronList", "CronDelete"])
      const schema = ToolJsonSchema.fromSchema(CreateParameters) as {
        properties: Record<string, unknown>
        required: string[]
      }
      expect(Object.keys(schema.properties).toSorted()).toEqual(["cron", "durable", "prompt", "recurring"])
      expect(schema.required.toSorted()).toEqual(["cron", "prompt"])
    }),
  )

  it.live("jitter is fixed per job and bounded", () =>
    Effect.gen(function* () {
      const fire = new Date(2026, 9, 3, 9, 0)
      const hour = new Date(2026, 9, 3, 10, 0)
      const day = new Date(2026, 9, 4, 9, 0)
      for (const id of ["a1b2c3d4", "ffffffff", "00000000", "deadbeef"]) {
        expect(SessionCron.fraction(id)).toBeGreaterThanOrEqual(0)
        expect(SessionCron.fraction(id)).toBeLessThan(1)
        expect(SessionCron.fraction(id)).toBe(SessionCron.fraction(id))

        // Recurring: late by up to a tenth of the period, capped at 15 minutes.
        const hourly = SessionCron.jittered({ id, recurring: true, fire, following: hour }) - fire.getTime()
        expect(hourly).toBeGreaterThanOrEqual(0)
        expect(hourly).toBeLessThan(minutes(6))
        const daily = SessionCron.jittered({ id, recurring: true, fire, following: day }) - fire.getTime()
        expect(daily).toBeGreaterThanOrEqual(0)
        expect(daily).toBeLessThan(minutes(15))

        // One-shot: early by up to 90 s on :00 and :30, exact otherwise.
        const sharp = fire.getTime() - SessionCron.jittered({ id, recurring: false, fire })
        expect(sharp).toBeGreaterThanOrEqual(0)
        expect(sharp).toBeLessThan(90_000)
        const off = new Date(2026, 9, 3, 8, 57)
        expect(SessionCron.jittered({ id, recurring: false, fire: off })).toBe(off.getTime())
      }
      expect(SessionCron.fraction("a1b2c3d4")).not.toBe(SessionCron.fraction("deadbeef"))
    }),
  )

  it.effect("a recurring job fires on every match", () =>
    Effect.gen(function* () {
      const test = yield* setup()
      const result = yield* test.create({ cron: "*/5 * * * *", prompt: "check the queue" })

      expect(result.title).toBe("Every */5 * * * *")
      expect(result.output).toContain(`Scheduled recurring job ${result.metadata.id} (*/5 * * * *).`)
      expect(result.output).toContain("auto-expires after 7 days")
      // Five minutes from the epoch, plus at most 30 s of jitter.
      expect(result.metadata.next).toBeGreaterThanOrEqual(minutes(5))
      expect(result.metadata.next).toBeLessThan(minutes(5) + 30_000)

      yield* TestClock.adjust("4 minutes")
      expect(test.sent).toEqual([])
      yield* TestClock.adjust("2 minutes")
      expect(test.texts()).toEqual(["check the queue"])
      // Neither part is shown in the transcript: the user did not type them.
      expect(test.sent[0].parts).toMatchObject([
        { type: "text", text: "check the queue", synthetic: true },
        { type: "text", synthetic: true },
      ])
      expect(test.sent[0].parts[1]).toMatchObject({
        text: expect.stringContaining(`scheduled job ${result.metadata.id} (cron "*/5 * * * *")`),
      })

      yield* TestClock.adjust("10 minutes")
      expect(test.texts()).toEqual(["check the queue", "check the queue", "check the queue"])
    }).pipe(withTmpdirInstance()),
  )

  it.effect("a one-shot job fires once and deletes itself", () =>
    Effect.gen(function* () {
      const test = yield* setup()
      const result = yield* test.create({ cron: "7 * * * *", prompt: "stand up", recurring: false, durable: true })
      expect(result.title).toBe("Once 7 * * * *")
      // An off-minute one-shot is not jittered.
      expect(result.metadata.next).toBe(minutes(7))
      expect((yield* test.list()).metadata.count).toBe(1)

      yield* TestClock.adjust("3 hours")
      expect(test.texts()).toEqual(["stand up"])
      expect((yield* test.list()).output).toBe("No scheduled jobs.")
    }).pipe(withTmpdirInstance()),
  )

  it.effect("lists and deletes jobs", () =>
    Effect.gen(function* () {
      const test = yield* setup()
      const first = yield* test.create({ cron: "*/10 * * * *", prompt: "poll the deploy" })
      const second = yield* test.create({ cron: "7 * * * *", prompt: "hourly report", recurring: false })

      const listed = yield* test.list()
      expect(listed.title).toBe("2 scheduled jobs")
      const lines = listed.output.split("\n")
      expect(lines[0]).toStartWith(`${second.metadata.id} — 7 * * * * (one-shot), next run `)
      expect(lines[0]).toEndWith(": hourly report")
      expect(lines[1]).toStartWith(`${first.metadata.id} — */10 * * * * (recurring), next run `)

      expect((yield* test.remove(first.metadata.id)).output).toBe(`Cancelled job ${first.metadata.id}.`)
      expect(yield* failure(test.remove(first.metadata.id))).toContain(
        `No scheduled job found with ID: ${first.metadata.id}`,
      )
      yield* TestClock.adjust("1 hour")
      expect(test.texts()).toEqual(["hourly report"])
    }).pipe(withTmpdirInstance()),
  )

  it.effect("fires that come due mid-turn wait for idle and collapse into one", () =>
    Effect.gen(function* () {
      const test = yield* setup()
      const runs = yield* SessionRunState.Service
      const scope = yield* Scope.Scope
      const hold = yield* Deferred.make<void>()
      yield* runs
        .ensureRunning(test.chat.id, Effect.succeed(test.reply), Deferred.await(hold).pipe(Effect.as(test.reply)))
        .pipe(Effect.forkIn(scope, { startImmediately: true }))
      const job = yield* test.create({ cron: "*/5 * * * *", prompt: "check the queue" })

      // Three matches pass while the session is busy.
      yield* TestClock.adjust("16 minutes")
      expect(test.sent).toEqual([])
      expect((yield* test.wake.pending(test.chat.id)).map((timer) => timer.key)).toEqual([`cron:${job.metadata.id}`])

      yield* Deferred.succeed(hold, undefined)
      yield* TestClock.adjust("2 seconds")
      expect(test.texts()).toEqual(["check the queue"])
    }).pipe(withTmpdirInstance()),
  )

  it.effect("a recurring job fires one last time after 7 days, then is deleted", () =>
    Effect.gen(function* () {
      const test = yield* setup()
      yield* test.create({ cron: "7 3 * * *", prompt: "nightly check" })

      yield* TestClock.adjust("6 days")
      const before = test.sent.length
      expect(before).toBeGreaterThanOrEqual(5)
      expect((yield* test.list()).metadata.count).toBe(1)
      expect(JSON.stringify(test.sent.at(-1)?.parts)).not.toContain("final run")

      yield* TestClock.adjust("2 days")
      expect(test.sent.length).toBeGreaterThan(before)
      expect(JSON.stringify(test.sent.at(-1)?.parts)).toContain(
        "This was its final run: recurring jobs expire after 7 days.",
      )
      expect((yield* test.list()).output).toBe("No scheduled jobs.")

      const count = test.sent.length
      yield* TestClock.adjust("3 days")
      expect(test.sent).toHaveLength(count)
    }).pipe(withTmpdirInstance()),
  )

  it.effect("survives the user stopping the session, but not the session being deleted", () =>
    Effect.gen(function* () {
      const test = yield* setup()
      yield* test.create({ cron: "*/5 * * * *", prompt: "check the queue" })
      yield* test.wake.clear(test.chat.id)
      yield* TestClock.adjust("6 minutes")
      expect(test.texts()).toEqual(["check the queue"])

      yield* (yield* Session.Service).remove(test.chat.id)
      yield* TestClock.adjust("1 hour")
      expect(test.sent).toHaveLength(1)
      expect(yield* (yield* SessionCron.Service).list(test.chat.id)).toEqual([])
    }).pipe(withTmpdirInstance()),
  )

  it.effect("rejects bad expressions, empty prompts and a 51st job", () =>
    Effect.gen(function* () {
      const test = yield* setup()
      expect(yield* failure(test.create({ cron: "every day", prompt: "x" }))).toContain("expected 5 fields")
      expect(yield* failure(test.create({ cron: "61 * * * *", prompt: "x" }))).toContain('bad minute field "61"')
      expect(yield* failure(test.create({ cron: "0 0 31 2 *", prompt: "x" }))).toContain("never matches a date")
      expect(yield* failure(test.create({ cron: "* * * * *", prompt: "  " }))).toContain("prompt is required")
      expect((yield* test.list()).metadata.count).toBe(0)

      yield* Effect.forEach(Array.from({ length: 50 }), () => test.create({ cron: "7 3 * * *", prompt: "x" }), {
        discard: true,
      })
      expect(yield* failure(test.create({ cron: "7 3 * * *", prompt: "x" }))).toContain(
        "This session already has 50 scheduled jobs",
      )
    }).pipe(withTmpdirInstance()),
  )
})
