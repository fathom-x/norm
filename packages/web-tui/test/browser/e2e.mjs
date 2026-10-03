// norm in the browser, end to end: a fresh visitor sets up a wallet on the
// setup screen, chats in the real TUI through the real owallet-web (wasm)
// against a mock Overpay, then comes back and unlocks.
//
//   setup: password → new wallet → new Overpay account (NIP-98 register)
//   → TUI home → prompt → the marketplace reply streams into the session
//   → reload → unlock (a wrong password first) → TUI → session list
//   → a second tab waits until the first closes
//   → (mock only) another visitor links an existing account via the popup
//
// Needs the page built with owallet-web in it: `bun run build:owallet` (Rust,
// clang, wasm-bindgen), then `vite build`. `bun run test:e2e` does both.
// Mock Overpay: owallet/crates/owallet-web/tests/mock-overpay/server.mjs.
//
// Env: CHROME, PLAYWRIGHT, PORT (default 4320), DIST — as in tui.mjs.
//
// Against a real Overpay instead of the mock (the staging smoke test in
// norm-web-ci.yml): E2E_OVERPAY_URL=<rails url> and E2E_MNEMONIC=<seed phrase
// of a wallet whose Overpay account holds core credits>. The wallet is
// imported instead of generated, and "Create a new Overpay account" finds
// that account again (NIP-98 sign-up is find-or-create by the wallet's key).
// That Overpay must list this page's origin (http://127.0.0.1:<PORT>) in
// API_CORS_ORIGINS. Mock-only assertions (exact reply, account number) are
// skipped there.
import assert from "node:assert/strict"
import { spawn } from "node:child_process"
import { mkdirSync } from "node:fs"
import { createRequire } from "node:module"
import path from "node:path"
import { fileURLToPath } from "node:url"

const root = path.resolve(fileURLToPath(new URL("../..", import.meta.url)))
const repo = path.resolve(root, "../..")
const shots = path.join(root, "test/screenshots")
const port = Number(process.env.PORT ?? 4320)
const base = `http://127.0.0.1:${port}`
const playwright = createRequire(import.meta.url)(
  process.env.PLAYWRIGHT ?? "/opt/node22/lib/node_modules/playwright/index.js",
)
const executablePath = process.env.CHROME ?? "/opt/pw-browsers/chromium-1194/chrome-linux/chrome"
const dist = process.env.DIST ?? path.join(root, "dist")
const PASSWORD = "correct horse battery staple"
const REAL_OVERPAY = process.env.E2E_OVERPAY_URL
const MNEMONIC = process.env.E2E_MNEMONIC
if (REAL_OVERPAY && !MNEMONIC) throw new Error("E2E_OVERPAY_URL needs E2E_MNEMONIC (a funded wallet)")

// Console errors that are expected and harmless. Keep each one explained.
const ALLOWED_ERRORS = [
  // The wrong-password step: owallet-web answers /_mgmt/unlock with 401, and
  // Chromium logs every failed fetch. The setup screen shows its own message.
  /401 \(Unauthorized\)/,
]

mkdirSync(shots, { recursive: true })

/** Start a child process and resolve with the first stdout line matching `ready`. */
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

const mock = REAL_OVERPAY
  ? undefined
  : start(
      process.execPath,
      [path.join(repo, "owallet/crates/owallet-web/tests/mock-overpay/server.mjs")],
      // A brand-new account starts at $0; this mock offers $1.00 of demo
      // credits, like staging with DEMO_CREDITS_CENTS set.
      {
        env: {
          ...process.env,
          PORT: "0",
          MOCK_STREAM_POLLS: "3",
          MOCK_CREDIT_CENTS: "0",
          MOCK_DEMO_CREDITS_CENTS: "100",
        },
      },
      /listening on (http:\/\/\S+)/,
    )
const preview = start(
  path.join(root, "node_modules/.bin/vite"),
  ["preview", "--port", String(port), "--strictPort", "--outDir", dist],
  { cwd: root },
  new RegExp(String(port)),
)
const overpay = REAL_OVERPAY ?? (await mock.line)[1]
await preview.line

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
  const heading = (text) => page.getByRole("heading", { name: text, exact: true }).waitFor({ timeout: 120_000 })
  const shot = (name) => page.screenshot({ path: path.join(shots, name) })
  const url = `${base}/?overpay=${encodeURIComponent(overpay)}`

  await step("first visit: choose the wallet password", async () => {
    await page.goto(url)
    await heading("Set up your wallet")
    await shot("e2e-01-setup-password.png")
    await page.getByLabel("Admin password").fill(PASSWORD)
    await page.getByLabel("Confirm password").fill(PASSWORD)
    await page.getByRole("button", { name: "Create wallet" }).click()
  })

  await step(
    MNEMONIC ? "import the funded wallet's seed phrase" : "create a new wallet (no seed phrase shown)",
    async () => {
      await heading("Create or import a wallet")
      if (!MNEMONIC) {
        await page.getByRole("button", { name: "Create a new wallet" }).click()
        return
      }
      await page.getByRole("button", { name: "Import a seed phrase" }).click()
      await heading("Import a seed phrase")
      await page.getByLabel("Seed phrase").fill(MNEMONIC)
      await page.getByRole("button", { name: "Import" }).click()
    },
  )

  await step("create an Overpay account and see its account number once", async () => {
    await heading("Connect to Overpay")
    assert.match(await page.textContent("main"), new RegExp(new URL(overpay).hostname.replaceAll(".", "\\.")))
    await page.getByRole("button", { name: "Create a new Overpay account" }).click()
    await heading("Your Overpay account")
    assert.match(await page.textContent("main"), REAL_OVERPAY ? /Account number: [\d ]{16,}/ : /1234567890123456/)
    await shot("e2e-02-setup-account.png")
    await page.getByRole("button", { name: "Continue" }).click()
  })

  await step(
    REAL_OVERPAY ? "a funded account needs no credits step" : "a $0 account takes the one-time demo credits",
    async () => {
      const credits = page.getByRole("heading", { name: "Add marketplace credits", exact: true })
      if (REAL_OVERPAY) {
        // Safety net: the staging wallet should hold credits, but if it ran dry
        // take demo credits when offered, else start anyway (the turn will say).
        // Either the credits screen or the TUI comes next; wait for whichever.
        const deadline = Date.now() + 120_000
        while (Date.now() < deadline) {
          if (await credits.isVisible().catch(() => false)) {
            const demo = page.getByRole("button", { name: /demo credits/ })
            await ((await demo.count()) ? demo : page.getByRole("button", { name: /Start norm/ })).first().click()
            return
          }
          if (/Ask anything/.test(await screen())) return
          await page.waitForTimeout(250)
        }
        throw new Error("neither the credits screen nor the TUI appeared")
      }
      await credits.waitFor({ timeout: 60_000 })
      assert.match(await page.textContent("main"), /\$1\.00 of demo credits/)
      await shot("e2e-02b-demo-credits.png")
      await page.getByRole("button", { name: "Add $1.00 of demo credits" }).click()
    },
  )

  await step("the TUI starts once the wallet is ready", async () => {
    const text = await waitFor(/Ask anything/, 120_000)
    assert.match(text, /\(oo\)/)
    assert.equal(await page.locator("main").count(), 0, "the setup screen is gone")
  })

  await step("a prompt is answered by the marketplace through owallet-web", async () => {
    await page.keyboard.type("say hello")
    await page.keyboard.press("Enter")
    // The mock's exact reply; a real model's is unknown, so wait for the
    // turn's footer (agent · model · duration) instead.
    const text = await waitFor(REAL_OVERPAY ? /Build · .+ · \d+(\.\d+)?m?s/ : /Hello from the mock seller\./, 180_000)
    assert.match(text, /say hello/)
    assert.doesNotMatch(text, /\[owallet error\]/)
    await shot("e2e-03-chat.png")
  })

  await step("the turn's real charge reaches the sidebar", async () => {
    // owallet reports usage.charged_cents; norm spends it instead of a token
    // estimate. The mock charges at most what was authorized.
    const text = await waitFor(/\$0\.0[1-9] spent/, 60_000)
    assert.match(text, /owallet/)
    assert.match(text, /owallet\s+Connected/, "the owallet MCP server is connected")
    assert.match(text, /core credits \$\d/)
    // No on-chain wallet and no dashboard in the browser: neither is a warning or a link.
    assert.doesNotMatch(text, /balance unavailable/)
    assert.doesNotMatch(text, /owallet\.internal\/wallet/)
    await shot("e2e-04-sidebar.png")
  })

  await step("a later visit asks for the password; a wrong one is refused", async () => {
    await page.reload()
    await heading("Unlock your wallet")
    await page.getByLabel("Admin password").fill("not the password")
    await page.getByRole("button", { name: "Unlock" }).click()
    await page.getByText("didn't unlock the wallet").waitFor({ timeout: 30_000 })
    await shot("e2e-05-unlock-wrong.png")
    await page.getByLabel("Admin password").fill(PASSWORD)
    await page.getByRole("button", { name: "Unlock" }).click()
  })

  await step("unlocked, the TUI comes back with the earlier session", async () => {
    await waitFor(/Ask anything/, 120_000)
    await page.keyboard.press("Control+x")
    await page.keyboard.press("l")
    const text = await waitFor(/Sessions/, 15_000)
    assert.match(text, /hello/i)
    await shot("e2e-06-session-list.png")
  })

  await step("a second tab waits for the first, then takes over when it closes", async () => {
    const second = await context.newPage()
    second.on("pageerror", (error) => errors.push(`pageerror (second tab): ${error.stack ?? error.message}`))
    await second.goto(url)
    await second.getByText("norm is open in another tab").waitFor({ timeout: 30_000 })
    await second.screenshot({ path: path.join(shots, "e2e-07-second-tab.png") })
    await page.close()
    await second.getByRole("heading", { name: "Unlock your wallet", exact: true }).waitFor({ timeout: 60_000 })
  })

  if (!REAL_OVERPAY) {
    await step("another visitor links an existing account through the Overpay login popup", async () => {
      // A fresh profile (its own OPFS). The mock's /oauth/authorize approves
      // at once and redirects to public/oauth/callback.html, which hands the
      // code back over a BroadcastChannel (the popup has no opener).
      const visitor = await browser.newContext({ viewport: { width: 1400, height: 860 } })
      const tab = await visitor.newPage()
      tab.on("pageerror", (error) => errors.push(`pageerror (visitor): ${error.stack ?? error.message}`))
      await tab.goto(url)
      const visitorHeading = (text) =>
        tab.getByRole("heading", { name: text, exact: true }).waitFor({ timeout: 120_000 })
      await visitorHeading("Set up your wallet")
      await tab.getByLabel("Admin password").fill(PASSWORD)
      await tab.getByLabel("Confirm password").fill(PASSWORD)
      await tab.getByRole("button", { name: "Create wallet" }).click()
      await visitorHeading("Create or import a wallet")
      await tab.getByRole("button", { name: "Create a new wallet" }).click()
      await visitorHeading("Connect to Overpay")
      await tab.getByRole("button", { name: "Use my existing Overpay account" }).click()
      await visitorHeading("Log in to Overpay")
      const [popup] = await Promise.all([
        visitor.waitForEvent("page"),
        tab.getByRole("button", { name: "Open Overpay login" }).click(),
      ])
      assert.equal(await popup.evaluate(() => window.opener), null, "the login popup has no handle on the norm tab")
      // Linked: past the setup screen (the credits step may come first).
      const credits = tab.getByRole("button", { name: /demo credits|Start norm/ })
      const tui = async () => {
        const deadline = Date.now() + 120_000
        while (Date.now() < deadline) {
          if (
            await credits
              .first()
              .isVisible()
              .catch(() => false)
          )
            await credits.first().click()
          const text = await tab.evaluate(() => {
            const buffer = globalThis.__norm?.term?.buffer.active
            return buffer
              ? Array.from(
                  { length: buffer.length },
                  (_, row) => buffer.getLine(row)?.translateToString(true) ?? "",
                ).join("\n")
              : ""
          })
          if (/Ask anything/.test(text)) return
          await tab.waitForTimeout(250)
        }
        throw new Error("the TUI did not start after linking")
      }
      await tui()
      await tab.screenshot({ path: path.join(shots, "e2e-08-oauth-linked.png") })
      await visitor.close()
    })
  }

  assert.deepEqual(errors, [], "no page or console errors")
} finally {
  console.log(results.join("\n"))
  if (errors.length) console.log(errors.join("\n"))
  await browser.close()
  preview.child.kill()
  mock?.child.kill()
}
