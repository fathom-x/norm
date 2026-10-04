import { describe, expect, test } from "bun:test"
import type { Message, Part } from "@opencode-ai/sdk/v2"
import { agentWritten } from "../../src/util/norm-notification"
import { pendingLabel } from "../../src/util/norm-pending"
import { queuedCount } from "../../src/util/norm-queue"

const text = (value: string, synthetic = true) => ({ type: "text", text: value, synthetic }) as Part

const event = [
  "<task-notification>",
  "<task-id>mon_1</task-id>",
  '<summary>Monitor event: "errors in deploy.log"</summary>',
  "<event>ERROR boom\nERROR again\nERROR third</event>",
  "</task-notification>",
].join("\n")

describe("util.norm-notification", () => {
  test("recognises messages the user did not write", () => {
    // A monitor event, and a scheduled prompt with its note for the model.
    expect(agentWritten([text(event)])).toBe(true)
    expect(agentWritten([text("check the deploy"), text("This message was sent by the wakeup")])).toBe(true)
  })

  test("leaves the user's own messages alone", () => {
    expect(agentWritten(undefined)).toBe(false)
    expect(agentWritten([])).toBe(false)
    expect(agentWritten([text("hello", false)])).toBe(false)
    // Typed text with a hidden attachment note beside it.
    expect(agentWritten([text("look at this", false), text("Called the Read tool")])).toBe(false)
    // An attachment or a command with no typed text is still the user's.
    expect(agentWritten([{ type: "file", mime: "image/png", url: "file:///a.png" } as Part])).toBe(false)
    expect(agentWritten([text("note"), { type: "subtask" } as Part])).toBe(false)
  })

  test("a waiting agent-written message is not a queued user message", () => {
    const message = (id: string, role: "user" | "assistant", completed?: number) =>
      ({ id, role, time: { created: 1, completed } }) as Message
    const messages = [message("u1", "user"), message("a1", "assistant"), message("n1", "user"), message("u2", "user")]
    const parts = { u1: [text("hi", false)], n1: [text(event)], u2: [text("also this", false)] }

    expect(queuedCount(messages, parts)).toBe(1)
    expect(queuedCount(messages.slice(0, 3), parts)).toBe(0)
    // Without parts every waiting user message counts, as before.
    expect(queuedCount(messages)).toBe(2)
  })
})

describe("util.norm-pending", () => {
  const now = 1_800_000_000_000

  test("labels what will start a turn on its own", () => {
    const task = { id: "mon_1", type: "monitor", description: "errors" }
    expect(pendingLabel({ wakeups: [{ key: "wakeup", at: now + 4 * 60_000 }], tasks: [] }, now)).toBe("wake 4m")
    expect(pendingLabel({ wakeups: [{ key: "wakeup", at: now + 185_000 }], tasks: [task, task] }, now)).toBe(
      "wake 4m · 2 bg",
    )
    expect(pendingLabel({ wakeups: [], tasks: [task] }, now)).toBe("1 bg")
    // Due, and waiting for the session to go idle.
    expect(pendingLabel({ wakeups: [{ key: "wakeup", at: now - 5_000 }], tasks: [] }, now)).toBe("wake <1m")
    // The soonest of several.
    expect(
      pendingLabel(
        {
          wakeups: [
            { key: "cron:a", at: now + 30 * 60_000 },
            { key: "wakeup", at: now + 2 * 60_000 },
          ],
          tasks: [],
        },
        now,
      ),
    ).toBe("wake 2m")
  })

  test("says nothing when nothing is pending", () => {
    expect(pendingLabel(undefined, now)).toBeUndefined()
    expect(pendingLabel({ wakeups: [], tasks: [] }, now)).toBeUndefined()
  })
})
