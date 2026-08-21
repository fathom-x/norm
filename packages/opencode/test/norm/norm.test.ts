import { test, expect, beforeEach, afterEach } from "bun:test"
import { Norm } from "@/norm/norm"
import { Global } from "@opencode-ai/core/global"
import path from "path"
import fs from "fs/promises"
import os from "os"
import { existsSync } from "fs"

// The bundled path derives from $HOME and the system path from $PATH, so each
// test builds a disposable home + PATH and restores the real ones after.
let home: string
let originalHome: string | undefined
let originalPath: string | undefined
let originalDisable: string | undefined
// NORM_HOME, NORM_OWALLET_URL and owallet's own vars redirect the paths and
// URL under test; a developer with any of them exported would otherwise see
// these tests resolve into their own sandbox.
const SANDBOX_VARS = [
  "NORM_HOME",
  "NORM_OWALLET_URL",
  "OWALLET_HOME",
  "OWALLET_DB_PATH",
  "OWALLET_CONFIG_DIR",
] as const
let originalSandbox: Record<string, string | undefined> = {}

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
  originalSandbox = Object.fromEntries(SANDBOX_VARS.map((key) => [key, process.env[key]]))
  for (const key of SANDBOX_VARS) delete process.env[key]
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
  for (const key of SANDBOX_VARS) {
    const value = originalSandbox[key]
    if (value === undefined) delete process.env[key]
    else process.env[key] = value
  }
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

test("NORM_HOME moves the wallet db, the bundled binary and the serve port", async () => {
  const sandbox = path.join(home, "sandbox")
  const outside = Norm.owalletUrl()
  process.env.NORM_HOME = sandbox

  expect(Norm.normHome()).toBe(sandbox)
  expect(Norm.owalletDbPath()).toBe(path.join(sandbox, "owallet", "owallet.db"))
  expect(Norm.bundledOwalletPath()).toBe(path.join(sandbox, "bin", "owallet"))

  // Its own port: reusing the default would hand the sandbox whatever serve is
  // already running against the real wallet. Stable across launches — and a
  // leftover NORM_OWALLET_URL is ignored inside a sandbox for the same
  // reason (that is exactly how a sandboxed norm once reached the real
  // wallet's serve); it still wins outside one.
  const url = Norm.owalletUrl()
  expect(url).not.toBe(outside)
  expect(url).toBe(Norm.owalletUrl())
  expect(Number(new URL(url).port)).toBeGreaterThanOrEqual(8800)
  process.env.NORM_OWALLET_URL = "http://127.0.0.1:9999"
  try {
    expect(Norm.owalletUrl()).toBe(url)
    delete process.env.NORM_HOME
    expect(Norm.owalletUrl()).toBe("http://127.0.0.1:9999")
  } finally {
    delete process.env.NORM_OWALLET_URL
    process.env.NORM_HOME = sandbox
  }
})

test("NORM_HOME is a relative-safe absolute root", async () => {
  process.env.NORM_HOME = "./relative-sandbox"
  expect(Norm.normHome()).toBe(path.resolve("./relative-sandbox"))
  process.env.NORM_HOME = "  "
  expect(Norm.normHome()).toBeUndefined()
})

test("applySandboxEnv points owallet's own vars at the sandbox", async () => {
  // Without NORM_HOME it leaves the environment alone.
  Norm.applySandboxEnv()
  expect(process.env.OWALLET_DB_PATH).toBeUndefined()

  const sandbox = path.join(home, "sandbox")
  process.env.NORM_HOME = sandbox
  Norm.applySandboxEnv()
  const dir = path.join(sandbox, "owallet")
  expect(process.env.OWALLET_HOME).toBe(dir)
  expect(process.env.OWALLET_DB_PATH).toBe(path.join(dir, "owallet.db"))
  expect(process.env.OWALLET_CONFIG_DIR).toBe(dir)
  // `owallet init` scaffolds *.owallet config files into OWALLET_CONFIG_DIR
  // and won't create it itself.
  expect(existsSync(dir)).toBe(true)

  // An OWALLET_* pointing outside the sandbox is refused — a leftover export
  // from earlier experiments must not punch through the isolation.
  process.env.OWALLET_DB_PATH = "/somewhere/else.db"
  Norm.applySandboxEnv()
  expect(process.env.OWALLET_DB_PATH).toBe(path.join(dir, "owallet.db"))

  // A path already inside the sandbox is kept (sandbox-consistent).
  const custom = path.join(dir, "renamed.db")
  process.env.OWALLET_DB_PATH = custom
  Norm.applySandboxEnv()
  expect(process.env.OWALLET_DB_PATH).toBe(custom)
})

test("a sandbox wallet is set up independently of the real one", async () => {
  delete process.env.NORM_DISABLE
  const real = path.join(home, ".owallet", "owallet.db")
  await fs.mkdir(path.dirname(real), { recursive: true })
  await fs.writeFile(real, "")

  const sandbox = path.join(home, "sandbox")
  process.env.NORM_HOME = sandbox
  await makeBinary(Norm.bundledOwalletPath())
  // The real DB exists, but the sandbox's does not — so first-run setup runs,
  // and it runs against the sandbox.
  expect(await Norm.needsWalletSetup()).toBe(true)
  expect(Norm.owalletDbPath()).not.toBe(real)
})

test("a sandbox picks its owallet without prompting", async () => {
  delete process.env.NORM_DISABLE
  await makeBinary(system())
  const sandbox = path.join(home, "sandbox")
  process.env.NORM_HOME = sandbox

  // Nothing installed in the sandbox: fall through to the one on PATH, which
  // is what makes "fresh state, existing binary" a one-variable setup.
  expect(await Norm.needsOwalletChoice()).toBe(false)
  expect(Norm.resolveOwalletBinary(undefined)).toBe(system())

  // Once the sandbox has its own, that one wins — no prompt, no recorded choice.
  await makeBinary(Norm.bundledOwalletPath())
  expect(Norm.resolveOwalletBinary(undefined)).toBe(path.join(sandbox, "bin", "owallet"))
  expect(await Norm.needsOwalletChoice()).toBe(false)
})

test("overpay-authorized marker merges with the auto-setup marker", async () => {
  expect(await Norm.readOverpayAuthorized()).toBeUndefined()
  await Norm.recordAutoSetup(true)
  await Norm.recordOverpayAuthorized(false)
  // Both facts survive in the one setup file.
  expect(await Norm.readOverpayAuthorized()).toBe(false)
  expect(await Norm.readAutoSetupDefaultPassword()).toBe(true)
  await Norm.recordOverpayAuthorized(true)
  expect(await Norm.readOverpayAuthorized()).toBe(true)
})
