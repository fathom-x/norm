// The norm TUI in the browser (M1 exit criteria), against the built page
// (`vite build` first; `bun run test:tui` does both), headless Chromium:
//
//   home screen → prompt + Enter → scripted reply in the session view →
//   command palette → owallet sidebar → reload → session list.
//
// Reads xterm.js's buffer (`window.__norm.term`), fails on any page error or
// console error not on the allowlist below. Screenshots: test/screenshots/.
//
// Env: CHROME, PLAYWRIGHT, PORT (default 4319), DIST — as in smoke.mjs;
// BASE_PATH for a build served under a sub-path.
import assert from "node:assert/strict"
import { spawn } from "node:child_process"
import { mkdirSync } from "node:fs"
import { createRequire } from "node:module"
import path from "node:path"
import { fileURLToPath } from "node:url"

const root = path.resolve(fileURLToPath(new URL("../..", import.meta.url)))
const shots = path.join(root, "test/screenshots")
const port = Number(process.env.PORT ?? 4319)
// BASE_PATH: the page built for a sub-path (NORM_WEB_BASE=/norm/ vite build),
// as on GitHub Pages; empty for the root.
const basePath = (process.env.BASE_PATH ?? "").replace(/\/+$/, "")
const base = `http://127.0.0.1:${port}${basePath}`
const playwright = createRequire(import.meta.url)(
  process.env.PLAYWRIGHT ?? "/opt/node22/lib/node_modules/playwright/index.js",
)
const executablePath = process.env.CHROME ?? "/opt/pw-browsers/chromium-1194/chrome-linux/chrome"

// Console errors that are expected and harmless. Keep each one explained.
const ALLOWED_ERRORS = [
  // None today.
]

mkdirSync(shots, { recursive: true })
// DIST: the build to serve (default dist/; `vite build --outDir <dir>` elsewhere
// keeps parallel builds from overwriting each other).
const dist = process.env.DIST ?? path.join(root, "dist")
const server = spawn(
  path.join(root, "node_modules/.bin/vite"),
  ["preview", "--port", String(port), "--strictPort", "--outDir", dist, "--base", `${basePath}/`],
  {
  cwd: root,
    stdio: ["ignore", "pipe", "inherit"],
  },
)
await new Promise((resolve, reject) => {
  server.stdout.on("data", (chunk) => String(chunk).includes(String(port)) && resolve())
  server.on("exit", (code) => reject(new Error(`vite preview exited (${code})`)))
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

try {
  const context = await browser.newContext({ viewport: { width: 1400, height: 860 } })
  const page = await context.newPage()
  page.on("pageerror", (error) => errors.push(`pageerror: ${error.stack ?? error.message}`))
  page.on("console", (message) => {
    if (message.type() !== "error") return
    const text = message.text()
    if (ALLOWED_ERRORS.some((pattern) => pattern.test(text))) return
    errors.push(`console.error: ${text}`)
  })

  const screen = () =>
    page.evaluate(() => {
      const buffer = globalThis.__norm?.term?.buffer.active
      if (!buffer) return ""
      return Array.from({ length: buffer.length }, (_, row) => buffer.getLine(row)?.translateToString(true) ?? "").join(
        "\n",
      )
    })
  const waitFor = async (pattern, timeout = 60_000) => {
    const deadline = Date.now() + timeout
    let last = ""
    while (Date.now() < deadline) {
      if (errors.length) throw new Error(errors.join("\n"))
      last = await screen()
      if (pattern.test(last)) return last
      await page.waitForTimeout(250)
    }
    throw new Error(`timed out waiting for ${pattern}; screen:\n${last}`)
  }
  const shot = (name) => page.screenshot({ path: path.join(shots, name) })
  const key = (combo) => page.keyboard.press(combo)

  await step("home screen renders in xterm (logo, prompt placeholder)", async () => {
    await page.goto(`${base}/?mock-owallet`)
    const text = await waitFor(/Ask anything/, 120_000)
    // norm's logo (packages/tui/src/logo.ts) and the footer keybind hints.
    assert.match(text, /\(oo\)/)
    assert.match(text, /ctrl\+p commands/)
    assert.match(text, /\/workspace/)
    await shot("tui-01-home.png")
  })

  await step("a prompt creates a session and the scripted reply streams in", async () => {
    await page.keyboard.type("write a note, then read it back")
    await key("Enter")
    const text = await waitFor(/Done\. Last tool result/, 90_000)
    assert.match(text, /write a note, then read it back/)
    assert.match(text, /hello from the scripted model/)
    await shot("tui-02-session.png")
  })

  await step("the sidebar shows owallet's status", async () => {
    const text = await waitFor(/core credits \$5\.00/, 60_000)
    assert.match(text, /owallet/)
    assert.match(text, /mock-seller \$2\.50/)
    await shot("tui-03-sidebar.png")
  })

  await step("ctrl+p opens the command palette", async () => {
    await key("Control+p")
    const text = await waitFor(/Commands/, 15_000)
    assert.match(text, /Switch model|Switch session|New session/i)
    await shot("tui-04-palette.png")
    await key("Escape")
  })

  await step("the terminal follows the viewport size", async () => {
    const cols = () => page.evaluate(() => globalThis.__norm.term.cols)
    const before = await cols()
    await page.setViewportSize({ width: 1000, height: 860 })
    await page.waitForFunction((previous) => globalThis.__norm.term.cols < previous, before)
    // opentui hears about it (process.stdout + SIGWINCH) and re-renders.
    await page.waitForFunction(() => globalThis.process.stdout.columns === globalThis.__norm.term.cols)
    await waitFor(/Done\. Last tool result/)
    await page.setViewportSize({ width: 1400, height: 860 })
    await page.waitForFunction((previous) => globalThis.__norm.term.cols === previous, before)
  })

  await step("after a reload the session is in the session list", async () => {
    await page.reload()
    await waitFor(/Ask anything/, 120_000)
    await key("Control+x")
    await key("l")
    const text = await waitFor(/Sessions/, 15_000)
    assert.match(text, /Scripted tool run|write a note/)
    await shot("tui-05-session-list.png")
  })

  assert.deepEqual(errors, [], "no page or console errors")
} finally {
  console.log(results.join("\n"))
  if (errors.length) console.log(errors.join("\n"))
  await browser.close()
  server.kill()
}
