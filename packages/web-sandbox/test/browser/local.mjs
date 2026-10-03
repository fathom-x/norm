// The demo service end to end with the LOCAL provider, headless Chromium:
//
//   landing (both cards; the browser card hidden without BROWSER_DIST) →
//   /sandbox/ shows the program's READY line → typing echoes → a resize
//   reaches the PTY (`stty size`) → a reload resumes the SAME process (its
//   counter keeps counting) → a second tab takes over (the first is told) →
//   "Start over" gives a fresh process.
//
// The "sandbox" is a scripted bash program in a PTY, so nothing here needs
// norm, owallet or E2B. Fails on any page error or console error. Screenshots:
// test/screenshots/local-*.png.
//
// Env: CHROME, PLAYWRIGHT (as in packages/web-tui's browser tests), PORT
// (default 4331), DIST (the built pages, default dist/web).
import assert from "node:assert/strict"
import { spawn } from "node:child_process"
import { mkdirSync, mkdtempSync, rmSync } from "node:fs"
import { createRequire } from "node:module"
import { tmpdir } from "node:os"
import path from "node:path"
import { fileURLToPath } from "node:url"

const root = path.resolve(fileURLToPath(new URL("../..", import.meta.url)))
const shots = path.join(root, "test/screenshots")
const port = Number(process.env.PORT ?? 4331)
const base = `http://127.0.0.1:${port}`
const playwright = createRequire(import.meta.url)(
  process.env.PLAYWRIGHT ?? "/opt/node22/lib/node_modules/playwright/index.js",
)
const executablePath = process.env.CHROME ?? "/opt/pw-browsers/chromium-1194/chrome-linux/chrome"

// Console errors that are expected and harmless. Keep each one explained.
const ALLOWED_ERRORS = [
  // None today.
]

// The stand-in for norm: says READY with its pid, then answers each line with
// a running counter (state that only survives if the process does).
const SCRIPT = [
  'n=0; echo "READY pid=$$"',
  'while IFS= read -r line; do n=$((n+1)); case "$line" in',
  '  size) echo "[$n] SIZE $(stty size)";;',
  '  *) echo "[$n] you said: $line";;',
  "esac; done",
].join("\n")

mkdirSync(shots, { recursive: true })
const sandboxRoot = mkdtempSync(path.join(tmpdir(), "web-sandbox-e2e-"))
const server = spawn("bun", ["server/main.ts"], {
  cwd: root,
  stdio: ["ignore", "pipe", "inherit"],
  env: {
    ...process.env,
    HOST: "127.0.0.1",
    PORT: String(port),
    SANDBOX_PROVIDER: "local",
    SANDBOX_COMMAND: "bash",
    SANDBOX_ARGS: JSON.stringify(["-c", SCRIPT]),
    SANDBOX_ROOT: sandboxRoot,
    WEB_DIST: process.env.DIST ?? path.join(root, "dist/web"),
    BROWSER_DIST: "",
    PAUSE_GRACE_MS: "60000",
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
const step = async (name, fn) => {
  const started = Date.now()
  try {
    await fn()
    results.push(`ok   ${name} (${Date.now() - started} ms)`)
  } catch (error) {
    results.push(`FAIL ${name}: ${error.message}`)
    throw error
  }
}

function watch(page, label) {
  page.on("pageerror", (error) => errors.push(`${label} pageerror: ${error.stack ?? error.message}`))
  page.on("console", (message) => {
    if (message.type() !== "error") return
    const text = message.text()
    if (ALLOWED_ERRORS.some((pattern) => pattern.test(text))) return
    errors.push(`${label} console.error: ${text}`)
  })
  return page
}

const screen = (page) =>
  page.evaluate(() => {
    const buffer = globalThis.__sandbox?.term?.buffer.active
    if (!buffer) return ""
    const lines = []
    for (let i = 0; i < buffer.length; i++) lines.push(buffer.getLine(i)?.translateToString(true) ?? "")
    return lines.join("\n")
  })

async function waitForScreen(page, pattern, what, timeout = 10_000) {
  const end = Date.now() + timeout
  let text = ""
  while (Date.now() < end) {
    text = await screen(page)
    if (pattern.test(text)) return text
    await page.waitForTimeout(50)
  }
  throw new Error(`timed out waiting for ${what} (${pattern}); screen:\n${text}`)
}

async function typeLine(page, line) {
  await page.locator("#terminal").click()
  await page.keyboard.type(line)
  await page.keyboard.press("Enter")
}

const overlayText = (page) => page.evaluate(() => (document.getElementById("overlay")?.hidden ? "" : document.getElementById("overlay-text")?.textContent ?? ""))

try {
  const context = await browser.newContext({ viewport: { width: 1200, height: 760 } })
  const page = watch(await context.newPage(), "tab1")

  await step("landing: both cards, browser card hidden without BROWSER_DIST", async () => {
    await page.goto(`${base}/`)
    await page.waitForSelector("#card-sandbox")
    assert.equal(await page.locator("h1").textContent(), "norm")
    assert.match(await page.locator("#card-browser h2").textContent(), /In your browser/)
    assert.match(await page.locator("#card-sandbox h2").textContent(), /In a cloud sandbox/)
    await page.waitForFunction(() => document.getElementById("card-browser")?.hidden === true)
    assert.equal(await page.locator("#card-sandbox").isVisible(), true)
    assert.equal(await page.locator("#browser-note").isVisible(), true)
    await page.screenshot({ path: path.join(shots, "local-01-landing.png") })
    await page.setViewportSize({ width: 390, height: 760 })
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth)
    assert.equal(overflow, false, "no horizontal scroll at phone width")
    await page.screenshot({ path: path.join(shots, "local-02-landing-phone.png") })
    await page.setViewportSize({ width: 1200, height: 760 })
  })

  let pid
  await step("/sandbox/: the program's first output appears", async () => {
    await page.click("#card-sandbox")
    await page.waitForURL(`${base}/sandbox/`)
    const text = await waitForScreen(page, /READY pid=\d+/, "READY")
    pid = /READY pid=(\d+)/.exec(text)[1]
    await page.waitForFunction(() => document.getElementById("overlay")?.hidden === true)
    const cookies = await context.cookies()
    const sid = cookies.find((cookie) => cookie.name === "sid")
    assert.ok(sid, "session cookie set")
    assert.equal(sid.httpOnly, true)
    assert.equal(sid.sameSite, "Lax")
  })

  await step("typing reaches the program and its answer comes back", async () => {
    await typeLine(page, "hello")
    await waitForScreen(page, /\[1\] you said: hello/, "echo")
    await page.screenshot({ path: path.join(shots, "local-03-typing.png") })
  })

  await step("a resize reaches the PTY", async () => {
    const before = await page.evaluate(() => [globalThis.__sandbox.term.cols, globalThis.__sandbox.term.rows])
    await page.setViewportSize({ width: 820, height: 520 })
    await page.waitForFunction(([cols]) => globalThis.__sandbox.term.cols !== cols, before)
    const [cols, rows] = await page.evaluate(() => [globalThis.__sandbox.term.cols, globalThis.__sandbox.term.rows])
    await typeLine(page, "size")
    await waitForScreen(page, new RegExp(`\\[2\\] SIZE ${rows} ${cols}`), `SIZE ${rows} ${cols}`)
    await page.setViewportSize({ width: 1200, height: 760 })
  })

  await step("a reload resumes the same process", async () => {
    await page.reload()
    const text = await waitForScreen(page, /READY pid=\d+/, "replayed output")
    assert.equal(/READY pid=(\d+)/.exec(text)[1], pid, "same process after reload")
    assert.match(text, /\[1\] you said: hello/)
    await page.waitForFunction(() => document.getElementById("overlay")?.hidden === true)
    await typeLine(page, "again")
    await waitForScreen(page, /\[3\] you said: again/, "counter kept counting")
    await page.screenshot({ path: path.join(shots, "local-04-resumed.png") })
  })

  const page2 = watch(await context.newPage(), "tab2")
  await step("a second tab takes over; the first is told", async () => {
    await page2.goto(`${base}/sandbox/`)
    const text = await waitForScreen(page2, /\[3\] you said: again/, "history in the second tab")
    assert.equal(/READY pid=(\d+)/.exec(text)[1], pid)
    await page.waitForFunction(() => document.getElementById("overlay-text")?.textContent === "Open in another tab")
    assert.equal(await overlayText(page), "Open in another tab")
    assert.equal(await page.locator("#overlay-action").textContent(), "Use it here")
    await typeLine(page2, "from two")
    await waitForScreen(page2, /\[4\] you said: from two/, "second tab answers")
    await page.screenshot({ path: path.join(shots, "local-05-replaced.png") })
    await page2.screenshot({ path: path.join(shots, "local-06-second-tab.png") })
  })

  await step("Start over gives a fresh process", async () => {
    page2.once("dialog", (dialog) => {
      assert.match(dialog.message(), /deletes your sandbox/)
      void dialog.accept()
    })
    await page2.click("#start-over")
    const text = await waitForScreen(page2, new RegExp(`READY pid=(?!${pid}\\b)\\d+`), "a new READY")
    const fresh = /READY pid=(\d+)/.exec(text)[1]
    assert.notEqual(fresh, pid, "a different process")
    assert.doesNotMatch(text, /you said: from two/, "old output gone")
    await page2.waitForFunction(() => document.getElementById("overlay")?.hidden === true)
    await typeLine(page2, "fresh")
    await waitForScreen(page2, /\[1\] you said: fresh/, "counter restarted")
    await page2.screenshot({ path: path.join(shots, "local-07-start-over.png") })
  })

  await step("no page or console errors", async () => {
    assert.deepEqual(errors, [])
  })
} finally {
  console.log(results.join("\n"))
  if (errors.length) console.log(errors.join("\n"))
  await browser.close()
  server.kill("SIGTERM")
  rmSync(sandboxRoot, { recursive: true, force: true })
}
