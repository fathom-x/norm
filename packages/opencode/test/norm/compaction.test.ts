import { test, expect, describe, beforeEach, afterEach } from "bun:test"
import fs from "fs/promises"
import { Norm } from "@/norm/norm"
import { NormCompaction } from "@opencode-ai/core/norm-compaction"

describe("NormCompaction storage", () => {
  beforeEach(() => fs.rm(NormCompaction.file(), { force: true }))
  afterEach(() => fs.rm(NormCompaction.file(), { force: true }))

  test("unset by default; set/get round-trips; clearing removes it", async () => {
    expect(await NormCompaction.get()).toBeUndefined()
    await NormCompaction.set("overpay/openai/gpt-5-mini")
    expect(await NormCompaction.get()).toBe("overpay/openai/gpt-5-mini")
    await NormCompaction.set(undefined)
    expect(await NormCompaction.get()).toBeUndefined()
  })

  test("a malformed file reads as unset", async () => {
    await fs.writeFile(NormCompaction.file(), "{not json")
    expect(await NormCompaction.get()).toBeUndefined()
  })
})

describe("Norm.applyCompactionModel", () => {
  const config = (extra: any = {}): any => ({
    provider: { overpay: { models: { default: {}, "openai/gpt-5-mini": {} } } },
    ...extra,
  })

  test("sets the compaction agent's model, keeping its other settings", () => {
    const cfg = config({ agent: { compaction: { temperature: 0.2 } } })
    Norm.applyCompactionModel(cfg, "overpay/openai/gpt-5-mini")
    expect(cfg.agent.compaction).toEqual({ temperature: 0.2, model: "overpay/openai/gpt-5-mini" })
  })

  test("the user's own config wins", () => {
    const cfg = config({ agent: { compaction: { model: "overpay/default" } } })
    Norm.applyCompactionModel(cfg, "overpay/openai/gpt-5-mini")
    expect(cfg.agent.compaction.model).toBe("overpay/default")
  })

  test("a model the provider no longer lists is skipped, not applied", () => {
    const cfg = config()
    Norm.applyCompactionModel(cfg, "overpay/retired/model")
    expect(cfg.agent).toBeUndefined()
  })

  test("nothing set, nothing changed", () => {
    const cfg = config()
    Norm.applyCompactionModel(cfg, undefined)
    expect(cfg.agent).toBeUndefined()
  })
})
