// End to end through norm's real layer against script/fake-owallet.ts: a
// `norm run` subprocess in a NORM_HOME sandbox — bootstrap, provider key,
// budget headers, real-spend cost, budget refusal — with only owallet faked.
import { test, expect, beforeEach, afterEach } from "bun:test"
import path from "path"
import fs from "fs/promises"
import os from "os"
import { Norm } from "@/norm/norm"
import { startFakeOwallet, prepareSandbox } from "../../script/fake-owallet"

const ENTRY = path.join(import.meta.dir, "..", "..", "src", "index.ts")

let root: string
let project: string
let fake: ReturnType<typeof startFakeOwallet>

function env(): Record<string, string> {
  const out: Record<string, string> = {}
  for (const [k, v] of Object.entries(process.env)) if (v !== undefined) out[k] = v
  // The test preload turns norm off and keeps the DB in memory; the
  // subprocesses need the real layer and a DB that outlives one invocation.
  for (const k of [
    "NORM_DISABLE",
    "OPENCODE_DB",
    "NORM_OWALLET_URL",
    "OWALLET_HOME",
    "OWALLET_DB_PATH",
    "OWALLET_CONFIG_DIR",
    "OWALLET_PASSWORD",
  ])
    delete out[k]
  return { ...out, NORM_HOME: root, OPENCODE_DISABLE_AUTOUPDATE: "1", OPENCODE_DISABLE_MODELS_FETCH: "1" }
}

async function norm(args: string[]) {
  const proc = Bun.spawn(["bun", "--conditions=browser", ENTRY, ...args], {
    cwd: project,
    env: env(),
    stdin: "ignore",
    stdout: "pipe",
    stderr: "pipe",
  })
  const [stdout, stderr, code] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
    proc.exited,
  ])
  return { code, stdout, stderr }
}

const events = (stdout: string) =>
  stdout
    .split("\n")
    .filter((line) => line.trim())
    .map((line) => JSON.parse(line) as Record<string, any>)

beforeEach(async () => {
  root = await fs.mkdtemp(path.join(os.tmpdir(), "norm-fake-owallet-"))
  project = path.join(root, "project")
  await fs.mkdir(project)
  const saved = process.env.NORM_HOME
  process.env.NORM_HOME = root
  const port = Number(new URL(Norm.owalletUrl()).port)
  if (saved === undefined) delete process.env.NORM_HOME
  else process.env.NORM_HOME = saved
  await prepareSandbox(root)
  fake = startFakeOwallet({ port })
})

afterEach(async () => {
  fake.stop()
  await fs.rm(root, { recursive: true, force: true })
})

test("a turn's cost is owallet's charge, and an exhausted budget refuses before spending", async () => {
  fake.reply({ text: "hi from the fake", charged_cents: 7, wallet_spent_cents: 20 })
  const first = await norm(["run", "--format", "json", "-m", "overpay/default", "hello"])
  expect(first.code, first.stderr).toBe(0)
  const turn = events(first.stdout)
  const sessionID = turn[0].sessionID
  expect(turn.find((e) => e.type === "text")?.part.text).toBe("hi from the fake")
  // charged_cents + wallet_spent_cents, not a token x list-price estimate.
  expect(turn.find((e) => e.type === "step_finish")?.part.cost).toBeCloseTo(0.27)

  const agentCall = fake.requests.find((r) => !r.housekeeping)!
  expect(agentCall.headers["x-owallet-spend-limit-usd"]).toBe("2.00")
  expect(agentCall.headers["x-owallet-request-max-usd"]).toBe("1.00")
  expect(agentCall.headers["x-session-id"]).toBe(sessionID)

  const budget = await norm(["budget", sessionID, "--set", "0.20"])
  expect(budget.code, budget.stderr).toBe(0)
  expect(JSON.parse(budget.stdout)).toMatchObject({ budget_usd: 0.2, remaining_usd: 0 })

  const spent = fake.spentCents
  const second = await norm(["run", "--format", "json", "-s", sessionID, "again"])
  const error = events(second.stdout).find((e) => e.type === "error")
  expect(JSON.stringify(error)).toContain("spending budget is used up")
  expect(fake.requests.at(-1)?.headers["x-owallet-spend-limit-usd"]).toBe("0.00")
  expect(fake.spentCents).toBe(spent)
}, 180_000)
