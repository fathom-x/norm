// The cloud-sandbox demo with the REAL native norm, end to end, headless
// Chromium — the E2B flow minus E2B: the broker's local provider runs the
// template's own wrapper (template/norm-demo.sh) in a PTY, which runs norm
// (from source) with owallet (native, built with `dev-envs`) against the
// mock Overpay, where a new account starts at $0 and demo credits are on
// offer:
//
//   /sandbox/ → norm's first run: "connect to Overpay" → 1 (new account, no
//   login) → demo credits → the TUI → a prompt is answered by the mock
//   seller through owallet → reload: the same norm process resumes.
//
// Env: OWALLET_BIN (default owallet/target/debug/owallet — build it with
// `cargo build -p owallet --features dev-envs` in owallet/), CHROME,
// PLAYWRIGHT, PORT (default 4332), DIST.
import assert from "node:assert/strict"
import { execFileSync, spawn } from "node:child_process"
import { chmodSync, existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { createRequire } from "node:module"
import { tmpdir } from "node:os"
import path from "node:path"
import { fileURLToPath } from "node:url"

const root = path.resolve(fileURLToPath(new URL("../..", import.meta.url)))
const repo = path.resolve(root, "../..")
const shots = path.join(root, "test/screenshots")
const port = Number(process.env.PORT ?? 4332)
const base = `http://127.0.0.1:${port}`
const playwright = createRequire(import.meta.url)(
  process.env.PLAYWRIGHT ?? "/opt/node22/lib/node_modules/playwright/index.js",
)
const executablePath = process.env.CHROME ?? "/opt/pw-browsers/chromium-1194/chrome-linux/chrome"
const owallet = path.resolve(process.env.OWALLET_BIN ?? path.join(repo, "owallet/target/debug/owallet"))
if (!existsSync(owallet)) throw new Error(`owallet not found at ${owallet} (cargo build -p owallet --features dev-envs)`)
const bun = execFileSync("which", ["bun"], { encoding: "utf8" }).trim()

// Console errors that are expected and harmless. Keep each one explained.
const ALLOWED_ERRORS = [
  // None today.
]

mkdirSync(shots, { recursive: true })
const scratch = mkdtempSync(path.join(tmpdir(), "web-sandbox-norm-"))

/** Start a child and resolve with the first stdout match of `ready`. */
function start(command, args, options, ready) {
  const child = spawn(command, args, { ...options, stdio: ["ignore", "pipe", "inherit"] })
  const line = new Promise((resolve, reject) => {
    let seen = ""
    child.stdout.on("data", (chunk) => {
      seen += String(chunk)
      const match = seen.match(ready)
      if (match) resolve(match)
    })
    child.on("exit", (code) => reject(new Error(`${command} exited (${code}) before it was ready`)))
  })
  return { child, line }
}

const mock = start(
  process.execPath,
  [path.join(repo, "owallet/crates/owallet-web/tests/mock-overpay/server.mjs")],
  { env: { ...process.env, PORT: "0", MOCK_STREAM_POLLS: "2", MOCK_CREDIT_CENTS: "0", MOCK_DEMO_CREDITS_CENTS: "100" } },
  /listening on (http:\/\/\S+)/,
)
const overpay = (await mock.line)[1]

// norm from source; the project is the provider's per-session workspace.
const normBin = path.join(scratch, "norm")
writeFileSync(
  normBin,
  `#!/bin/sh\nproject="$PWD"\ncd ${JSON.stringify(path.join(repo, "packages/opencode"))} && exec ${bun} run --conditions=browser src/index.ts "$project"\n`,
)
chmodSync(normBin, 0o755)

const server = start(
  bun,
  ["server/main.ts"],
  {
    cwd: root,
    env: {
      ...process.env,
      HOST: "127.0.0.1",
      PORT: String(port),
      SANDBOX_PROVIDER: "local",
      SANDBOX_COMMAND: "bash",
      SANDBOX_ARGS: JSON.stringify([path.join(root, "template/norm-demo.sh")]),
      SANDBOX_ROOT: path.join(scratch, "sandboxes"),
      SANDBOX_ENV: JSON.stringify({
        NORM_BIN: normBin,
        // owallet is found on PATH (the sandbox has no $NORM_HOME/bin here).
        PATH: [path.dirname(owallet), path.dirname(bun), "/usr/local/bin", "/usr/bin", "/bin"].join(":"),
        NORM_OWALLET_ENV: "staging",
        OVERPAY_RAILS_URL_STAGING: overpay,
        TERM: "xterm-256color",
        COLORTERM: "truecolor",
      }),
      WEB_DIST: process.env.DIST ?? path.join(root, "dist/web"),
      BROWSER_DIST: "",
    },
  },
  /listening/,
)
await server.line

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
  const waitFor = async (pattern, timeout = 120_000) => {
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

  await step("norm's first run asks how to connect to Overpay", async () => {
    await page.goto(`${base}/sandbox/`)
    const text = await waitFor(/Choice \[1\/2\/3\]/)
    assert.match(text, /Create a new Overpay account/)
    assert.doesNotMatch(text, /password/i, "OWALLET_PASSWORD is set per sandbox: no password prompt")
    await shot("norm-01-connect.png")
  })

  await step("a new account, no login, then the demo credits", async () => {
    await type("")
    const text = await waitFor(/Add them\? \[Y\/n\]/)
    assert.match(text, /Account number: \d{16}/)
    await type("y")
  })

  await step("the TUI starts, linked and funded", async () => {
    const text = await waitFor(/Ask anything/)
    assert.match(text, /\(oo\)/)
    await shot("norm-02-tui.png")
  })

  await step("a prompt is answered by the marketplace through native owallet", async () => {
    await type("say hello")
    const text = await waitFor(/Hello from the mock seller\./, 180_000)
    assert.match(text, /say hello/)
    // owallet's usage.charged_cents, not a token estimate, is what norm spends.
    await waitFor(/\$0\.0[1-9] spent/, 60_000)
    await shot("norm-03-chat.png")
  })

  await step("a reload resumes the same norm process", async () => {
    await page.reload()
    const text = await waitFor(/Hello from the mock seller\./, 60_000)
    assert.doesNotMatch(text, /Choice \[1\/2\/3\]/, "not a fresh first run")
    await shot("norm-04-resumed.png")
  })

  await step("no page or console errors", async () => assert.deepEqual(errors, []))
} finally {
  console.log(results.join("\n"))
  if (errors.length) console.log(errors.join("\n"))
  await browser.close()
  server.child.kill("SIGTERM")
  mock.child.kill("SIGTERM")
  rmSync(scratch, { recursive: true, force: true })
}
