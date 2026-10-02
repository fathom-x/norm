import { test, expect, describe, beforeEach, afterEach } from "bun:test"
import fs from "fs/promises"
import { Norm } from "@/norm/norm"
import { NormAgentModels } from "@opencode-ai/core/norm-agent-models"
import { NormPricing } from "@opencode-ai/core/norm-pricing"
import { SessionV1 } from "@opencode-ai/core/v1/session"
import { ProviderV2 } from "@opencode-ai/core/provider"
import { ModelV2 } from "@opencode-ai/core/model"
import type { Provider } from "@/provider/provider"
import { MessageV2 } from "../../src/session/message-v2"
import { SessionID, MessageID, PartID } from "../../src/session/schema"

describe("NormAgentModels storage", () => {
  const clear = () =>
    Promise.all((["compaction", "title"] as const).map((agent) => fs.rm(NormAgentModels.file(agent), { force: true })))
  beforeEach(clear)
  afterEach(clear)

  test("each agent is unset by default and round-trips its own choice", async () => {
    expect(await NormAgentModels.get("compaction")).toBeUndefined()
    expect(await NormAgentModels.get("title")).toBeUndefined()
    await NormAgentModels.set("compaction", "overpay/openai/gpt-5-mini")
    await NormAgentModels.set("title", NormAgentModels.CONVERSATION)
    expect(await NormAgentModels.get("compaction")).toBe("overpay/openai/gpt-5-mini")
    expect(await NormAgentModels.get("title")).toBe(NormAgentModels.CONVERSATION)
    await NormAgentModels.set("compaction", undefined)
    expect(await NormAgentModels.get("compaction")).toBeUndefined()
  })

  test("compaction keeps reading the file /compaction-model wrote before titles existed", () => {
    expect(NormAgentModels.file("compaction").endsWith("norm-compaction.json")).toBe(true)
  })

  test("a malformed file, or 'conversation' for compaction, reads as unset", async () => {
    await fs.writeFile(NormAgentModels.file("title"), "{not json")
    expect(await NormAgentModels.get("title")).toBeUndefined()
    await NormAgentModels.set("compaction", NormAgentModels.CONVERSATION)
    expect(await NormAgentModels.get("compaction")).toBeUndefined()
  })
})

const marketplace: NormPricing.Model[] = [
  { id: "default" },
  { id: "pricey/model", contextLength: 200_000, pricing: { input: 3, output: 15, min_charge: 0.01 } },
  { id: "cheap/model", contextLength: 1_000_000, pricing: { input: 0.07, output: 0.65, min_charge: 0.01 } },
  { id: "cheap/model:free", contextLength: 1_000_000, pricing: { input: 0, output: 0, min_charge: 0.01 } },
  { id: "tiny/window", contextLength: 8_000, pricing: { input: 0.01, output: 0.01, min_charge: 0.01 } },
  { id: "retired/model", active: false, contextLength: 128_000, pricing: { input: 0.01, output: 0.01, min_charge: 0.01 } },
]

describe("Norm.applyAgentModels", () => {
  const config = (extra: any = {}): any => ({
    provider: {
      overpay: {
        models: Object.fromEntries(marketplace.filter((m) => m.active !== false).map((m) => [m.id, {}])),
      },
    },
    ...extra,
  })

  test("titles default to the cheapest suitable model, with norm's title prompt", () => {
    expect(Norm.cheapestTitleModel(marketplace)).toBe("overpay/cheap/model")
    const cfg = config()
    Norm.applyAgentModels(cfg, {}, marketplace)
    expect(cfg.agent.title).toEqual({ model: "overpay/cheap/model", prompt: Norm.TITLE_PROMPT })
    expect(cfg.agent.compaction).toBeUndefined()
  })

  test("'same as the conversation' leaves the title model to opencode", () => {
    const cfg = config()
    Norm.applyAgentModels(cfg, { title: NormAgentModels.CONVERSATION }, marketplace)
    expect(cfg.agent.title.model).toBeUndefined()
    expect(cfg.agent.title.prompt).toBe(Norm.TITLE_PROMPT)
  })

  test("explicit choices apply, keeping the agent's other settings", () => {
    const cfg = config({ agent: { compaction: { temperature: 0.2 } } })
    Norm.applyAgentModels(cfg, { compaction: "overpay/pricey/model", title: "overpay/pricey/model" }, marketplace)
    expect(cfg.agent.compaction).toEqual({ temperature: 0.2, model: "overpay/pricey/model" })
    expect(cfg.agent.title.model).toBe("overpay/pricey/model")
  })

  test("the user's own config wins, prompt included", () => {
    const cfg = config({ agent: { compaction: { model: "overpay/default" }, title: { model: "x/y", prompt: "mine" } } })
    Norm.applyAgentModels(cfg, { compaction: "overpay/cheap/model" }, marketplace)
    expect(cfg.agent.compaction.model).toBe("overpay/default")
    expect(cfg.agent.title).toEqual({ model: "x/y", prompt: "mine" })
  })

  test("a model the provider no longer lists is skipped, not applied", () => {
    const cfg = config()
    Norm.applyAgentModels(cfg, { compaction: "overpay/retired/model", title: "overpay/retired/model" }, marketplace)
    expect(cfg.agent.compaction).toBeUndefined()
    expect(cfg.agent.title.model).toBeUndefined()
  })

  test("without a priced model list the title stays on the conversation's model", () => {
    const cfg = config()
    Norm.applyAgentModels(cfg, {}, undefined)
    expect(cfg.agent.title.model).toBeUndefined()
  })

  test("housekeeping agents ask owallet for plain completions", () => {
    expect([...Norm.PLAIN_AGENTS].sort()).toEqual(["compaction", "summary", "title"])
    expect(Norm.TOOLS_HEADER).toBe("x-owallet-tools")
  })
})

// An owallet refusal stored as an assistant reply must reach the model as a
// harness note, not as words in its own mouth.
describe("owallet errors in the model's history", () => {
  const sessionID = SessionID.make("session")
  const providerID = ProviderV2.ID.make("overpay")
  const model = {
    id: ModelV2.ID.make("m"),
    providerID,
    api: { id: "m", url: "", npm: "@ai-sdk/openai-compatible" },
    capabilities: { attachment: false, input: { image: false, pdf: false }, toolcall: true },
  } as unknown as Provider.Model
  const part = (messageID: string, id: string, text: string) =>
    ({
      id: PartID.make(`prt_${id}`),
      sessionID,
      messageID: MessageID.make(`msg_${messageID}`),
      type: "text",
      text,
    }) as SessionV1.Part
  const user = (id: string, text: string): SessionV1.WithParts => ({
    info: { id: `msg_${id}`, sessionID, role: "user", time: { created: 0 }, agent: "build", model: { providerID, modelID: "m" } } as any,
    parts: [part(id, id, text)],
  })
  const assistant = (id: string, text: string): SessionV1.WithParts => ({
    info: {
      id: `msg_${id}`,
      sessionID,
      role: "assistant",
      time: { created: 0 },
      parentID: "msg_u1",
      modelID: "m",
      providerID: "overpay",
      mode: "",
      agent: "build",
      path: { cwd: "/", root: "/" },
      cost: 0,
      tokens: { input: 0, output: 0, reasoning: 0, cache: { read: 0, write: 0 } },
      finish: "stop",
    } as any,
    parts: [part(id, id, text)],
  })
  const refusal =
    "\n\n[owallet error] This message (about 21,651 tokens of context on m) needs at least $0.05 — over your $0.01 per-message limit."

  test("a refused turn is dropped and explained on the next user message", async () => {
    const messages = await MessageV2.toModelMessages(
      [user("u1", "Testing compaction"), assistant("a1", refusal), user("u2", "were you able to see the error?")],
      model,
    )
    expect(messages.map((m) => m.role)).toEqual(["user", "user"])
    const second = messages[1].content as Array<{ type: string; text: string }>
    expect(second[0].text).toContain("<system-reminder>")
    expect(second[0].text).toContain("neither you nor the user wrote it")
    expect(second[0].text).toContain("over your $0.01 per-message limit")
    expect(second[0].text).not.toContain("[owallet error]")
    expect(second[1].text).toBe("were you able to see the error?")
  })

  test("a reply cut off by an error keeps what the model said", async () => {
    const messages = await MessageV2.toModelMessages(
      [user("u1", "hi"), assistant("a1", `Partial answer${refusal}`), user("u2", "go on")],
      model,
    )
    expect(messages.map((m) => m.role)).toEqual(["user", "assistant", "user"])
    expect(JSON.stringify(messages[1].content)).toContain("Partial answer")
    expect(JSON.stringify(messages[1].content)).not.toContain("owallet error")
    expect((messages[2].content as any)[0].text).toContain("<system-reminder>")
  })

  test("ordinary replies are untouched", async () => {
    const messages = await MessageV2.toModelMessages([user("u1", "hi"), assistant("a1", "hello"), user("u2", "ok")], model)
    expect(messages.map((m) => m.role)).toEqual(["user", "assistant", "user"])
    expect(JSON.stringify(messages[2].content)).not.toContain("system-reminder")
  })
})
