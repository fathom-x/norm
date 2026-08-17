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
})

afterEach(async () => {
  if (originalHome === undefined) delete process.env.HOME
  else process.env.HOME = originalHome
  if (originalPath === undefined) delete process.env.PATH
  else process.env.PATH = originalPath
  if (originalDisable === undefined) delete process.env.NORM_DISABLE
  else process.env.NORM_DISABLE = originalDisable
  await fs.rm(path.join(Global.Path.data, "owallet-binary.json"), { force: true })
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

test("needsOwalletChoice respects NORM_DISABLE", async () => {
  process.env.NORM_DISABLE = "1"
  await makeBinary(system())
  await makeBinary(bundled())
  expect(await Norm.needsOwalletChoice()).toBe(false)
})
