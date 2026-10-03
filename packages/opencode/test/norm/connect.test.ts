// norm's native "connect to Overpay" step (Norm.connectOverpay): the choice
// between a zero-click new account (`owallet register`, then demo credits on
// a $0 balance) and the browser login (`owallet authorize`), against a
// scripted fake owallet.
import { afterEach, beforeEach, expect, test } from "bun:test"
import fs from "fs/promises"
import os from "os"
import path from "path"
import { Global } from "@opencode-ai/core/global"
import { Norm } from "@/norm/norm"

let dir: string
let log: string
let stderr: string
const realWrite = process.stderr.write.bind(process.stderr)

/** A fake owallet: logs each call's args, answers per subcommand from env-tuned scripts. */
async function fakeOwallet(answers: Record<string, { stdout?: string; stderr?: string; code?: number }>) {
  const bin = path.join(dir, "owallet")
  const cases = Object.entries(answers)
    .map(
      ([key, a]) =>
        `  *"${key}"*) printf '%s' '${(a.stdout ?? "").replaceAll("'", "'\\''")}'; printf '%s' '${(a.stderr ?? "").replaceAll("'", "'\\''")}' >&2; exit ${a.code ?? 0};;`,
    )
    .join("\n")
  await fs.writeFile(
    bin,
    `#!/bin/sh\necho "$*" >> "${log}"\ncase "$*" in\n${cases}\n  *) echo "error: unrecognized subcommand" >&2; exit 2;;\nesac\n`,
    { mode: 0o755 },
  )
  return bin
}

const answers = (...replies: string[]) => {
  const queue = [...replies]
  return async () => queue.shift() ?? ""
}
const calls = async () => (await fs.readFile(log, "utf8").catch(() => "")).trim().split("\n").filter(Boolean)

beforeEach(async () => {
  dir = await fs.mkdtemp(path.join(os.tmpdir(), "norm-connect-test-"))
  log = path.join(dir, "calls.log")
  stderr = ""
  process.stderr.write = ((chunk: any) => {
    stderr += String(chunk)
    return true
  }) as typeof process.stderr.write
  await fs.rm(path.join(Global.Path.data, "owallet-setup.json"), { force: true })
})

afterEach(async () => {
  process.stderr.write = realWrite
  await fs.rm(path.join(Global.Path.data, "owallet-setup.json"), { force: true })
  await fs.rm(dir, { recursive: true, force: true })
})

test("default choice: a new account with no login, then the demo credits on a $0 balance", async () => {
  const bin = await fakeOwallet({
    "register --json": { stdout: '{"linked":true,"npub":"npub1x","account_number":"1111222233334444"}' },
    "demo-credits --claim --json": { stdout: '{"granted_cents":100,"balance_cents":100}' },
    "demo-credits --json": { stdout: '{"enabled":true,"amount_cents":100,"granted":false,"core_balance_cents":0}' },
  })
  let authorized = false
  const linked = await Norm.connectOverpay(bin, process.env, answers("", "y"), {
    authorize: async () => ((authorized = true), 0),
  })
  expect(linked).toBe(true)
  expect(authorized).toBe(false)
  expect(stderr).toContain("Account number: 1111222233334444")
  expect(stderr).toContain("Added $1.00 of demo credits")
  expect((await calls()).map((c) => c.replace(/^--(staging|dev) /, ""))).toEqual([
    "register --json",
    "demo-credits --json",
    "demo-credits --claim --json",
  ])
  expect(await Norm.readOverpayAuthorized()).toBe(true)
})

test("a funded account is not offered demo credits", async () => {
  const bin = await fakeOwallet({
    "register --json": { stdout: '{"linked":true,"npub":"npub1x"}' },
    "demo-credits --json": { stdout: '{"enabled":true,"amount_cents":100,"granted":false,"core_balance_cents":250}' },
  })
  expect(await Norm.connectOverpay(bin, process.env, answers("1"))).toBe(true)
  expect((await calls()).some((c) => c.includes("--claim"))).toBe(false)
})

test("no demo credits on offer: the Lightning hint instead", async () => {
  const bin = await fakeOwallet({
    "register --json": { stdout: '{"linked":true,"npub":"npub1x"}' },
    "demo-credits --json": { stdout: '{"enabled":false,"amount_cents":0,"granted":false,"core_balance_cents":0}' },
  })
  await Norm.connectOverpay(bin, process.env, answers("1"))
  expect(stderr).toContain("owallet credits load")
})

test("choice 2 logs in through the browser", async () => {
  const bin = await fakeOwallet({})
  let authorized = 0
  const linked = await Norm.connectOverpay(bin, process.env, answers("2"), {
    authorize: async () => (authorized++, 0),
  })
  expect(linked).toBe(true)
  expect(authorized).toBe(1)
  expect(await calls()).toEqual([])
})

test("an owallet without `register` falls back to the browser login", async () => {
  const bin = await fakeOwallet({})
  let authorized = 0
  expect(
    await Norm.connectOverpay(bin, process.env, answers(""), { authorize: async () => (authorized++, 0) }),
  ).toBe(true)
  expect(authorized).toBe(1)
})

test("a failed sign-up or 'later' leaves the wallet unlinked, to be offered again", async () => {
  const failing = await fakeOwallet({ "register --json": { stderr: "Overpay unreachable", code: 1 } })
  expect(await Norm.connectOverpay(failing, process.env, answers("1"))).toBe(false)
  expect(stderr).toContain("Overpay unreachable")
  expect(await Norm.readOverpayAuthorized()).toBe(false)
  expect(await Norm.connectOverpay(failing, process.env, answers("3"))).toBe(false)
  expect(await Norm.readOverpayAuthorized()).toBe(false)
})
