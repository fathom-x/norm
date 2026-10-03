// The local provider against a real PTY (bun-pty) running a small bash script.
import { afterEach, describe, expect, test } from "bun:test"
import { existsSync, mkdtempSync, readFileSync, rmSync, statSync } from "node:fs"
import { tmpdir } from "node:os"
import path from "node:path"
import { DEFAULT_PASS_ENV } from "../../server/config"
import { LocalProvider } from "../../server/providers/local"
import { until } from "./fakes"

const SID = "E".repeat(22)
const SCRIPT = [
  'echo "READY pid=$$ home=$NORM_HOME pw=${#OWALLET_PASSWORD} term=$TERM secret=${SESSION_SECRET:-none}"',
  'while IFS= read -r line; do case "$line" in size) echo "SIZE $(stty size)";; quit) exit 7;; *) echo "GOT $line";; esac; done',
].join("\n")

let roots: string[] = []
let providers: LocalProvider[] = []
afterEach(async () => {
  for (const provider of providers) await provider.close()
  for (const root of roots) rmSync(root, { recursive: true, force: true })
  roots = []
  providers = []
  delete process.env.SESSION_SECRET
})

function setup(idleKillMs = 0) {
  const root = mkdtempSync(path.join(tmpdir(), "web-sandbox-local-"))
  roots.push(root)
  process.env.SESSION_SECRET = "must-not-leak-into-the-pty-xxxxxxxxxx"
  const provider = new LocalProvider({
    command: "bash",
    args: ["-c", SCRIPT],
    root,
    env: {},
    passEnv: DEFAULT_PASS_ENV,
    idleKillMs,
    normOwalletEnv: "staging",
  })
  providers.push(provider)
  return { provider, root }
}

function collector() {
  const chunks: string[] = []
  const exits: Array<number | undefined> = []
  return {
    chunks,
    exits,
    text: () => chunks.join(""),
    options: (cols = 80, rows = 24) => ({
      cols,
      rows,
      onData: (bytes: Uint8Array) => void chunks.push(new TextDecoder().decode(bytes)),
      onExit: (code: number | undefined) => void exits.push(code),
    }),
  }
}

const send = (text: string) => new TextEncoder().encode(text)

describe("local provider", () => {
  test("starts the command in a PTY with per-visitor state", async () => {
    const { provider, root } = setup()
    const phases: string[] = []
    let admitted = 0
    const handle = await provider.findOrCreate(SID, {
      cols: 80,
      rows: 24,
      admit: () => void admitted++,
      onPhase: (p) => phases.push(p),
    })
    expect(admitted).toBe(1)
    expect(phases).toEqual(["creating"])
    const out = collector()
    const attachment = await handle.attach(out.options(91, 27))
    await until(() => out.text().includes("READY"), 5000, "READY")
    expect(out.text()).toContain(`home=${path.join(root, SID, "norm")}`)
    expect(out.text()).toContain("pw=32")
    expect(out.text()).toContain("term=xterm-256color")
    // Only allowlisted env reaches the program.
    expect(out.text()).toContain("secret=none")
    const pwFile = path.join(root, SID, ".owallet-password")
    expect(statSync(pwFile).mode & 0o777).toBe(0o600)
    expect(readFileSync(pwFile, "utf8")).toHaveLength(32)

    attachment.write(send("size\r"))
    await until(() => out.text().includes("SIZE 27 91"), 5000, "size")
    attachment.resize(100, 30)
    attachment.write(send("size\r"))
    await until(() => out.text().includes("SIZE 30 100"), 5000, "resized")
    expect(await provider.count()).toBe(1)
    expect(await provider.status(SID)).toBe("running")
  })

  test("detach keeps the process; reattach replays the backlog; reset starts fresh", async () => {
    const { provider, root } = setup()
    const first = collector()
    const a = await (await provider.findOrCreate(SID, { cols: 80, rows: 24, admit: () => {} })).attach(first.options())
    await until(() => first.text().includes("READY"), 5000)
    a.write(send("one\r"))
    await until(() => first.text().includes("GOT one"), 5000)
    const pid = /READY pid=(\d+)/.exec(first.text())![1]
    a.detach()

    const second = collector()
    let admitted = 0
    const b = await (
      await provider.findOrCreate(SID, { cols: 80, rows: 24, admit: () => void admitted++ })
    ).attach(second.options())
    expect(admitted).toBe(0)
    // The backlog: same process, earlier output included.
    expect(second.text()).toContain(`READY pid=${pid}`)
    expect(second.text()).toContain("GOT one")
    b.write(send("two\r"))
    await until(() => second.text().includes("GOT two"), 5000)
    expect(first.text()).not.toContain("GOT two")

    await provider.reset(SID)
    expect(existsSync(path.join(root, SID))).toBe(false)
    expect(await provider.status(SID)).toBe("none")
    const third = collector()
    let admittedAgain = 0
    await (await provider.findOrCreate(SID, { cols: 80, rows: 24, admit: () => void admittedAgain++ })).attach(
      third.options(),
    )
    expect(admittedAgain).toBe(1)
    await until(() => third.text().includes("READY"), 5000)
    expect(third.text()).not.toContain(`pid=${pid} `)
  })

  test("the program exiting reports its code; the next attach starts a new one", async () => {
    const { provider } = setup()
    const out = collector()
    const a = await (await provider.findOrCreate(SID, { cols: 80, rows: 24, admit: () => {} })).attach(out.options())
    await until(() => out.text().includes("READY"), 5000)
    a.write(send("quit\r"))
    await until(() => out.exits.length === 1, 5000, "exit")
    expect(out.exits).toEqual([7])
    expect(await provider.count()).toBe(0)
    expect(await provider.status(SID)).toBe("paused")

    const phases: string[] = []
    const again = collector()
    await (
      await provider.findOrCreate(SID, { cols: 80, rows: 24, admit: () => {}, onPhase: (p) => phases.push(p) })
    ).attach(again.options())
    expect(phases).toEqual(["resuming"])
    await until(() => again.text().includes("READY"), 5000)
  })

  test("idle kill after pause", async () => {
    const { provider } = setup(50)
    const out = collector()
    await (await provider.findOrCreate(SID, { cols: 80, rows: 24, admit: () => {} })).attach(out.options())
    await until(() => out.text().includes("READY"), 5000)
    await provider.pause(SID)
    await until(() => provider["terminals"].size === 0, 2000, "idle kill")
  })

  test("refuses a sid that is not a sid", async () => {
    const { provider } = setup()
    await expect(provider.findOrCreate("../etc", { cols: 80, rows: 24, admit: () => {} })).rejects.toThrow()
    await expect(provider.reset("../../")).rejects.toThrow()
  })
})
