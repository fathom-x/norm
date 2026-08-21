import { test, expect, beforeEach, afterEach } from "bun:test"
import { Norm } from "@/norm/norm"
import { Global } from "@opencode-ai/core/global"
import path from "path"
import fs from "fs/promises"
import os from "os"

// The bundled path derives from $HOME and the system path from $PATH, so each
// test builds a disposable home + PATH and restores the real ones after.
let home: string
let originalHome: string | undefined
let originalPath: string | undefined
let originalDisable: string | undefined

async function makeBinary(file: string) {
  await fs.mkdir(path.dirname(file), { recursive: true })
  await fs.writeFile(file, "#!/bin/sh\n", { mode: 0o755 })
}

beforeEach(async () => {
  home = await fs.mkdtemp(path.join(os.tmpdir(), "norm-owallet-test-"))
  originalHome = process.env.HOME
  originalPath = process.env.PATH
  originalDisable = process.env.NORM_DISABLE
  process.env.HOME = home
  process.env.PATH = path.join(home, "elsewhere")
  await fs.rm(path.join(Global.Path.data, "owallet-binary.json"), { force: true })
  await fs.rm(path.join(Global.Path.data, "owallet-setup.json"), { force: true })
})

afterEach(async () => {
  if (originalHome === undefined) delete process.env.HOME
  else process.env.HOME = originalHome
  if (originalPath === undefined) delete process.env.PATH
  else process.env.PATH = originalPath
  if (originalDisable === undefined) delete process.env.NORM_DISABLE
  else process.env.NORM_DISABLE = originalDisable
  await fs.rm(path.join(Global.Path.data, "owallet-binary.json"), { force: true })
  await fs.rm(path.join(Global.Path.data, "owallet-setup.json"), { force: true })
  await fs.rm(home, { recursive: true, force: true })
})

const bundled = () => path.join(home, ".norm", "bin", "owallet")
const system = () => path.join(home, "elsewhere", "owallet")

test("resolveOwalletBinary prefers a pre-existing install when no choice is recorded", async () => {
  await makeBinary(bundled())
  await makeBinary(system())
  expect(Norm.resolveOwalletBinary(undefined)).toBe(system())
})

test("resolveOwalletBinary honors the recorded choice", async () => {
  await makeBinary(bundled())
  await makeBinary(system())
  expect(Norm.resolveOwalletBinary("bundled")).toBe(bundled())
  expect(Norm.resolveOwalletBinary("system")).toBe(system())
})

test("resolveOwalletBinary falls back when the chosen binary is gone", async () => {
  await makeBinary(bundled())
  expect(Norm.resolveOwalletBinary("system")).toBe(bundled())
  await fs.rm(bundled())
  await makeBinary(system())
  expect(Norm.resolveOwalletBinary("bundled")).toBe(system())
})

test("choice roundtrips through the state file", async () => {
  expect(await Norm.readOwalletChoice()).toBeUndefined()
  await Norm.recordOwalletChoice("bundled")
  expect(await Norm.readOwalletChoice()).toBe("bundled")
})

test("needsOwalletChoice once a pre-existing install appears and nothing is recorded", async () => {
  delete process.env.NORM_DISABLE
  expect(await Norm.needsOwalletChoice()).toBe(false)
  // A bundled copy alone is not a choice — it's the only option.
  await makeBinary(bundled())
  expect(await Norm.needsOwalletChoice()).toBe(false)
  // A pre-existing install is enough even without the bundled copy on disk:
  // the installer only fetches the bundled one after a "bundled" choice is
  // recorded, so the prompt must not wait for it.
  await fs.rm(bundled())
  await makeBinary(system())
  expect(await Norm.needsOwalletChoice()).toBe(true)
  await makeBinary(bundled())
  expect(await Norm.needsOwalletChoice()).toBe(true)
  await Norm.recordOwalletChoice("system")
  expect(await Norm.needsOwalletChoice()).toBe(false)
})

test("needsWalletSetup wants a binary, no DB, and no prior decline", async () => {
  delete process.env.NORM_DISABLE
  // No owallet binary anywhere: nothing could create the wallet.
  expect(await Norm.needsWalletSetup()).toBe(false)
  await makeBinary(bundled())
  expect(await Norm.needsWalletSetup()).toBe(true)
  // An existing wallet database means there is nothing to set up.
  const db = path.join(home, ".owallet", "owallet.db")
  await fs.mkdir(path.dirname(db), { recursive: true })
  await fs.writeFile(db, "")
  expect(await Norm.needsWalletSetup()).toBe(false)
  // A recorded decline silences the offer for good.
  await fs.rm(db)
  expect(await Norm.needsWalletSetup()).toBe(true)
  await Norm.recordWalletSetupDeclined()
  expect(await Norm.needsWalletSetup()).toBe(false)
  expect(await Norm.readWalletSetupDeclined()).toBe(true)
})

test("auto-setup marker roundtrips and drives the default password", async () => {
  const originalPassword = process.env.OWALLET_PASSWORD
  delete process.env.OWALLET_PASSWORD
  try {
    expect(await Norm.readAutoSetupDefaultPassword()).toBe(false)

    // A wallet auto-created under an exported OWALLET_PASSWORD records
    // autoSetup without the default-password marker: nothing to restore.
    await Norm.recordAutoSetup(false)
    expect(await Norm.readAutoSetupDefaultPassword()).toBe(false)
    await Norm.applyAutoSetupPassword()
    expect(process.env.OWALLET_PASSWORD).toBeUndefined()

    // Default-password auto-setup restores the password on later launches…
    await Norm.recordAutoSetup(true)
    expect(await Norm.readAutoSetupDefaultPassword()).toBe(true)
    await Norm.applyAutoSetupPassword()
    // Read through a fresh lookup: TS narrows the direct property access to
    // undefined after the delete above (it can't see the mutation inside
    // applyAutoSetupPassword).
    expect(process.env["OWALLET_PASSWORD"] as string | undefined).toBe(Norm.DEFAULT_OWALLET_PASSWORD)

    // …but never overrides one the user exported themselves.
    process.env.OWALLET_PASSWORD = "user-picked"
    await Norm.applyAutoSetupPassword()
    expect(process.env.OWALLET_PASSWORD).toBe("user-picked")
  } finally {
    if (originalPassword === undefined) delete process.env.OWALLET_PASSWORD
    else process.env.OWALLET_PASSWORD = originalPassword
  }
})

test("needsWalletSetup respects NORM_DISABLE", async () => {
  process.env.NORM_DISABLE = "1"
  await makeBinary(bundled())
  expect(await Norm.needsWalletSetup()).toBe(false)
})

test("needsOwalletChoice respects NORM_DISABLE", async () => {
  process.env.NORM_DISABLE = "1"
  await makeBinary(system())
  await makeBinary(bundled())
  expect(await Norm.needsOwalletChoice()).toBe(false)
})

test("system prompt addendum rides overpay models only", async () => {
  const { SystemPrompt } = await import("@/session/system")
  const overpay = { providerID: "overpay", api: { id: "default" } } as any
  const anthropic = { providerID: "anthropic", api: { id: "claude-sonnet-4" } } as any

  delete process.env.NORM_DISABLE
  expect(SystemPrompt.provider(overpay).some((part) => part.includes("Overpay marketplace"))).toBe(true)
  expect(SystemPrompt.provider(anthropic).some((part) => part.includes("Overpay marketplace"))).toBe(false)

  process.env.NORM_DISABLE = "1"
  expect(SystemPrompt.provider(overpay).some((part) => part.includes("Overpay marketplace"))).toBe(false)
})
