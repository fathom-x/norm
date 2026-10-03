// The cloud-sandbox demo on REAL E2B against a real Overpay (staging by
// default): the broker's e2b provider, the norm-demo template, headless
// Chromium.
//
//   /sandbox/ → a sandbox is created → norm's first run → new account (no
//   login) → demo credits if offered → the TUI → a prompt gets a finished
//   turn → a reload resumes the SAME sandbox and norm process → Start over
//   deletes the sandbox (the test always cleans up).
//
// Skips (exit 0) without E2B_API_KEY. Needs the template built
// (`bun run template:build`). Spends a few cents of the new account's
// credits on the Overpay behind it, so that Overpay should offer demo
// credits (DEMO_CREDITS_CENTS) or the turn has nothing to pay with.
//
// Env: E2B_API_KEY, E2B_TEMPLATE (default norm-demo), OVERPAY_HOSTS (default
// the staging host), CHROME, PLAYWRIGHT, PORT (default 4333), DIST.
import assert from "node:assert/strict"
import { spawn } from "node:child_process"
import { mkdirSync } from "node:fs"
import { createRequire } from "node:module"
import path from "node:path"
import { fileURLToPath } from "node:url"
import { randomBytes } from "node:crypto"

if (!process.env.E2B_API_KEY) {
  console.log("skip: E2B_API_KEY is not set")
  process.exit(0)
}

const root = path.resolve(fileURLToPath(new URL("../..", import.meta.url)))
const shots = path.join(root, "test/screenshots")
const port = Number(process.env.PORT ?? 4333)
const base = `http://127.0.0.1:${port}`
const playwright = createRequire(import.meta.url)(
  process.env.PLAYWRIGHT ?? "/opt/node22/lib/node_modules/playwright/index.js",
)
const executablePath = process.env.CHROME ?? "/opt/pw-browsers/chromium-1194/chrome-linux/chrome"
const ALLOWED_ERRORS = [
  // None today.
]
mkdirSync(shots, { recursive: true })

const server = spawn("bun", ["server/main.ts"], {
  cwd: root,
  stdio: ["ignore", "pipe", "inherit"],
  env: {
    ...process.env,
    HOST: "127.0.0.1",
    PORT: String(port),
    SANDBOX_PROVIDER: "e2b",
    SESSION_SECRET: randomBytes(48).toString("base64url"),
    WEB_DIST: process.env.DIST ?? path.join(root, "dist/web"),
    BROWSER_DIST: "",
    RETENTION_DAYS: "0",
  },
})
await new Promise((resolve, reject) => {
  server.stdout.on("data", (chunk) => {
    process.stdout.write(chunk)
    if (String(chunk).includes("listening")) resolve()
  })
  server.on("exit", (code) => reject(new Error(`server exited (${code})`)))
})

const browser = await playwright.chromium.launch({ executablePath, args: ["--no-sandbox"] })
const results = []
const errors = []
const timings = {}
const step = async (name, fn) => {
  const started = Date.now()
  try {
    await fn()
    timings[name] = Date.now() - started
    results.push(`ok   ${name} (${Date.now() - started} ms)`)
  } catch (error) {
    results.push(`FAIL ${name}: ${error.message}`)
    throw error
  }
}
let page
try {
  const context = await browser.newContext({ viewport: { width: 1400, height: 860 } })
  page = await context.newPage()
  page.on("pageerror", (error) => errors.push(`pageerror: ${error.stack ?? error.message}`))
  page.on("console", (message) => {
    if (message.type() !== "error") return
    const text = message.text()
    if (!ALLOWED_ERRORS.some((pattern) => pattern.test(text))) errors.push(`console.error: ${text}`)
  })
  const screen = () =>
    page.evaluate(() => {
      const buffer = globalThis.__sandbox?.term?.buffer.active
      if (!buffer) return ""
      return Array.from({ length: buffer.length }, (_, i) => buffer.getLine(i)?.translateToString(true) ?? "").join(
        "\n",
      )
    })
  const waitFor = async (pattern, timeout = 180_000) => {
    const end = Date.now() + timeout
    let text = ""
    while (Date.now() < end) {
      if (errors.length) throw new Error(errors.join("\n"))
      text = await screen()
      if (pattern.test(text)) return text
      await page.waitForTimeout(250)
    }
    throw new Error(`timed out waiting for ${pattern}; screen:\n${text}`)
  }
  const shot = (name) => page.screenshot({ path: path.join(shots, name) })
  const type = async (text) => {
    await page.locator("#terminal").click()
    if (text) await page.keyboard.type(text)
    await page.keyboard.press("Enter")
  }

  await step("a sandbox is created and norm's first run asks how to connect", async () => {
    await page.goto(`${base}/sandbox/`)
    await waitFor(/Choice \[1\/2\/3\]/, 240_000)
    await shot("e2b-01-connect.png")
  })

  await step("new account with no login; demo credits when offered", async () => {
    await type("")
    const text = await waitFor(/Add them\? \[Y\/n\]|Ask anything|owallet credits load/)
    assert.match(text, /Account number: \d{16}/)
    if (/Add them\?/.test(text)) await type("y")
  })

  await step("the TUI starts", async () => {
    await waitFor(/Ask anything/)
    await shot("e2b-02-tui.png")
  })

  await step("a prompt gets a finished turn from the marketplace", async () => {
    await type("Reply with the single word: pong")
    const text = await waitFor(/Build · .+ · \d+(\.\d+)?m?s/, 240_000)
    assert.doesNotMatch(text, /\[owallet error\]/)
    await shot("e2b-03-chat.png")
  })

  await step("a reload resumes the same sandbox and norm process", async () => {
    await page.reload()
    const text = await waitFor(/pong|Reply with the single word/i, 120_000)
    assert.doesNotMatch(text, /Choice \[1\/2\/3\]/)
    await shot("e2b-04-resumed.png")
  })

  await step("no page or console errors", async () => assert.deepEqual(errors, []))
} finally {
  // Always delete the sandbox this run created.
  if (page)
    await page
      .evaluate(() => fetch("/api/reset", { method: "POST" }).then((r) => r.status))
      .then((status) => console.log(`cleanup: /api/reset → ${status}`))
      .catch((error) => console.log(`cleanup failed: ${error.message}`))
  console.log(results.join("\n"))
  console.log("timings (ms):", JSON.stringify(timings))
  if (errors.length) console.log(errors.join("\n"))
  await browser.close()
  server.kill("SIGTERM")
}
