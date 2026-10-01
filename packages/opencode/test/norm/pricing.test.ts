import { test, expect, describe } from "bun:test"
import { Norm } from "@/norm/norm"
import { NormPricing } from "@opencode-ai/core/norm-pricing"
import { usable } from "@/session/overflow"

// owallet >= the /v1/models pricing release: USD per Mtok, markup in.
const body = {
  object: "list",
  data: [
    { id: "default", object: "model" },
    {
      id: "anthropic/claude-sonnet-5",
      name: "Anthropic: Claude Sonnet 5",
      context_length: 200_000,
      active: true,
      pricing: {
        input: 3.6,
        output: 18,
        cache_read: 0.36,
        long_context: [{ min_input_tokens: 50_000, input: 7.2, output: 27 }],
        min_charge: 0.01,
        min_authorization: 0.11,
        basis: "list",
      },
    },
    { id: "gone/model", active: false, pricing: { input: 1, output: 1, min_charge: 0.01 } },
    { id: "unpriced/model" },
  ],
}

const sonnet = () => NormPricing.parseModels(body)![1].pricing!

describe("NormPricing.parseModels", () => {
  test("reads price, context and availability, and tolerates bare ids", () => {
    const models = NormPricing.parseModels(body)!
    expect(models.map((m) => m.id)).toEqual(["default", "anthropic/claude-sonnet-5", "gone/model", "unpriced/model"])
    expect(models[0].pricing).toBeUndefined()
    expect(models[1]).toMatchObject({ name: "Anthropic: Claude Sonnet 5", contextLength: 200_000, active: true })
    expect(models[1].pricing).toMatchObject({ input: 3.6, output: 18, cache_read: 0.36, min_authorization: 0.11 })
    expect(models[2].active).toBe(false)
    expect(models[3].pricing).toBeUndefined()
  })

  test("an older owallet's id-only list still parses; an empty one is undefined", () => {
    expect(NormPricing.parseModels({ data: [{ id: "default" }, { id: "x/y" }] })!.map((m) => m.id)).toEqual([
      "default",
      "x/y",
    ])
    expect(NormPricing.parseModels({ data: [] })).toBeUndefined()
    expect(NormPricing.parseModels(undefined)).toBeUndefined()
  })
})

describe("NormPricing estimates", () => {
  test("a step is rounded up to the cent and never below the per-turn minimum", () => {
    const cheap: NormPricing.Pricing = { input: 0.1, output: 0.4, min_charge: 0.01 }
    expect(NormPricing.estimateStep(cheap, { contextTokens: 2_000, outputTokens: 200 })).toBe(0.01)
    // 10k in at $3.60 + 1k out at $18 = 3.6¢ + 1.8¢ → 6¢
    expect(NormPricing.estimateStep(sonnet(), { contextTokens: 10_000, outputTokens: 1_000 })).toBe(0.06)
  })

  test("cached context is billed at the cache rate", () => {
    const fresh = NormPricing.estimateStep(sonnet(), { contextTokens: 40_000, outputTokens: 500 })
    const cached = NormPricing.estimateStep(sonnet(), {
      contextTokens: 40_000,
      cachedTokens: 38_000,
      outputTokens: 500,
    })
    expect(fresh).toBe(0.16)
    expect(cached).toBe(0.03)
  })

  test("a long prompt switches to the long-context tier", () => {
    expect(NormPricing.rates(sonnet(), 49_999)).toEqual({ input: 3.6, output: 18 })
    expect(NormPricing.rates(sonnet(), 50_000)).toEqual({ input: 7.2, output: 27 })
  })

  test("nextStep: same model uses the cache, another model reads it all fresh", () => {
    const turns: NormPricing.Turn[] = [
      { modelID: "anthropic/claude-sonnet-5", tokens: { input: 2_000, output: 1_000, cache: { read: 30_000, write: 0 } } },
    ]
    const same = NormPricing.nextStep(sonnet(), "anthropic/claude-sonnet-5", turns)!
    const other = NormPricing.nextStep(sonnet(), "openai/gpt-5", turns)!
    expect(same.switching).toBe(false)
    expect(other.switching).toBe(true)
    expect(other.usd).toBeGreaterThan(same.usd)
    expect(NormPricing.nextStep(sonnet(), "x", [])).toBeUndefined()
  })

  test("leastAuthorization mirrors owallet's refusal floor", () => {
    // Tiny prompt: the model's minimum commitment.
    expect(NormPricing.leastAuthorization(sonnet(), 100)).toBe(0.11)
    // 60k reported tokens ≈ 80k by owallet's byte count → long tier:
    // 80k × $7.20 + 256 × $27 = 57.6¢ + 0.69¢ → 59¢.
    expect(NormPricing.leastAuthorization(sonnet(), 60_000)).toBe(0.59)
  })

  test("perMillion keeps small rates readable", () => {
    expect(NormPricing.perMillion(sonnet())).toBe("$3.60/$18.00 per M")
    expect(NormPricing.perMillion({ input: 0.03, output: 0.12, min_charge: 0.01 })).toBe("$0.03/$0.12 per M")
    expect(NormPricing.perMillion({ input: 0.075, output: 0, min_charge: 0.01 })).toBe("$0.075/$0 per M")
  })
})

describe("Norm.modelConfig", () => {
  test("a priced model gets cost and a context limit", () => {
    const model = NormPricing.parseModels(body)![1]
    expect(Norm.modelConfig(model)).toEqual({
      name: "Anthropic: Claude Sonnet 5",
      cost: { input: 3.6, output: 18, cache_read: 0.36 },
      limit: { context: 200_000, output: 32_000 },
    })
  })

  test("bare ids get a name only; default keeps its label", () => {
    expect(Norm.modelConfig({ id: "x/y" })).toEqual({ name: "x/y" })
    expect(Norm.modelConfig({ id: "default" })).toEqual({ name: "Overpay marketplace (default)" })
  })

  test("mergeModels skips retired models and never overrides the user's entry", () => {
    const configured: Record<string, object> = {
      "anthropic/claude-sonnet-5": { name: "My Sonnet", limit: { context: 100_000, output: 8_000 } },
    }
    const merged = Norm.mergeModels(configured, NormPricing.parseModels(body)!)
    expect(Object.keys(merged).sort()).toEqual(["anthropic/claude-sonnet-5", "default", "unpriced/model"])
    expect(merged["anthropic/claude-sonnet-5"]).toEqual({
      name: "My Sonnet",
      limit: { context: 100_000, output: 8_000 },
      cost: { input: 3.6, output: 18, cache_read: 0.36 },
    })
  })

  // opencode never auto-compacts at limit.context 0, and with its 32k output
  // default a small window would leave no usable room and compact every turn.
  test("the context limit leaves compaction room even for small windows", () => {
    for (const context of [8_192, 16_000, 32_768, 128_000, 1_000_000]) {
      const limit = NormPricing.limit(context)
      const room = usable({ cfg: {} as any, model: { limit } as any })
      expect(room).toBeGreaterThan(context / 2)
    }
  })
})
