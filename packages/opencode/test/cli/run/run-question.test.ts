// norm: `run --ask` / `--answer` — the question tool from the command line.
// Subprocess tests against the scripted TestLLMServer; see run-process.test.ts
// for the harness conventions.
import { describe, expect } from "bun:test"
import { Effect } from "effect"
import { reply } from "../../lib/llm-server"
import { cliIt } from "../../lib/cli-process"

const QUESTION = {
  questions: [
    {
      question: "Which color?",
      header: "Color",
      options: [
        { label: "Red", description: "warm" },
        { label: "Blue", description: "cool" },
      ],
    },
  ],
}

const toolOutput = (events: Array<Record<string, any>>) =>
  events.find((e) => e.type === "tool_use" && e.part.tool === "question")?.part.state

describe("run --ask / --answer (norm)", () => {
  cliIt.concurrent(
    "--answer replies to a question asked during the run",
    ({ llm, opencode }) =>
      Effect.gen(function* () {
        yield* llm.push(reply().tool("question", QUESTION))
        yield* llm.text("thanks")
        const result = yield* opencode.run("pick a color", {
          format: "json",
          extraArgs: ["--answer", "Blue"],
        })
        opencode.expectExit(result, 0)
        const events = opencode.parseJsonEvents(result.stdout) as Array<Record<string, any>>
        expect(events.find((e) => e.type === "question")?.answers).toEqual([["Blue"]])
        const state = toolOutput(events)
        expect(state.status).toBe("completed")
        expect(state.output).toContain('"Which color?"="Blue"')
      }),
    60_000,
  )

  cliIt.concurrent(
    "a JSON array answers a multi-select question",
    ({ llm, opencode }) =>
      Effect.gen(function* () {
        yield* llm.push(reply().tool("question", QUESTION))
        yield* llm.text("thanks")
        const result = yield* opencode.run("pick colors", {
          format: "json",
          extraArgs: ["--answer", '["Red","Blue"]'],
        })
        opencode.expectExit(result, 0)
        const events = opencode.parseJsonEvents(result.stdout) as Array<Record<string, any>>
        expect(toolOutput(events).output).toContain('"Which color?"="Red, Blue"')
      }),
    60_000,
  )

  cliIt.concurrent(
    "--ask without an answer prints the question and exits 3",
    ({ llm, opencode }) =>
      Effect.gen(function* () {
        yield* llm.push(reply().tool("question", QUESTION))
        const result = yield* opencode.run("pick a color", { format: "json", extraArgs: ["--ask"] })
        opencode.expectExit(result, 3)
        const events = opencode.parseJsonEvents(result.stdout) as Array<Record<string, any>>
        const asked = events.find((e) => e.type === "question")
        expect(asked?.request.questions[0].question).toBe("Which color?")
        expect(asked?.answers).toBeUndefined()
      }),
    60_000,
  )

  cliIt.live(
    "attached, the question stays pending for a later --answer",
    ({ llm, opencode }) =>
      Effect.gen(function* () {
        yield* llm.push(reply().tool("question", QUESTION))
        yield* llm.text("thanks")
        const server = yield* opencode.serve()

        const first = yield* opencode.run("pick a color", {
          format: "json",
          extraArgs: ["--attach", server.url, "--ask"],
        })
        opencode.expectExit(first, 3)
        const sessionID = (opencode.parseJsonEvents(first.stdout)[0] as Record<string, any>).sessionID

        const second = yield* opencode.spawn([
          "run",
          "--attach",
          server.url,
          "--format",
          "json",
          "-s",
          sessionID,
          "--answer",
          "Red",
        ])
        opencode.expectExit(second, 0)
        const events = opencode.parseJsonEvents(second.stdout) as Array<Record<string, any>>
        expect(toolOutput(events).output).toContain('"Which color?"="Red"')
        expect(events.some((e) => e.type === "text" && e.part.text === "thanks")).toBe(true)
      }),
    90_000,
  )
})
