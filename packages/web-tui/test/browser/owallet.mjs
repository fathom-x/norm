// Browser test for the real owallet-web in the core worker, against the mock
// Overpay (owallet/crates/owallet-web/tests/mock-overpay/server.mjs). Build
// first: `bun run test:owallet` runs build:owallet + build + this.
//
//   node test/browser/owallet.mjs
//
// Env: CHROME (Chromium binary), PLAYWRIGHT (path to playwright's index.js),
// PORT (preview port, default 4318). Screenshots land in test/screenshots/.
//
// The page (its debug panel, `?debug-panel`) reaches owallet.internal through
// the worker's router (`privateFetch` RPC) after pointing it at the mock
// (`owallet.configure`, src/owallet.ts), and the core through `norm.api` as
// smoke.mjs does. Everything else — norm's
// bootstrap minting the key, the provider, the MCP client, the session loop —
// is the core's own code talking to the real wasm owallet.
import assert from "node:assert/strict"
import { spawn } from "node:child_process"
import { mkdirSync } from "node:fs"
import { createRequire } from "node:module"
import path from "node:path"
import { createInterface } from "node:readline"
import { fileURLToPath } from "node:url"

const root = path.resolve(fileURLToPath(new URL("../..", import.meta.url)))
const shots = path.join(root, "test/screenshots")
const port = Number(process.env.PORT ?? 4318)
const base = `http://127.0.0.1:${port}`
const playwright = createRequire(import.meta.url)(
  process.env.PLAYWRIGHT ?? "/opt/node22/lib/node_modules/playwright/index.js",
)
const executablePath = process.env.CHROME ?? "/opt/pw-browsers/chromium-1194/chrome-linux/chrome"
const PASSWORD = "browser-test-pw"

mkdirSync(shots, { recursive: true })

// The mock Overpay on a random port.
const mock = spawn(
  process.execPath,
  [path.resolve(root, "../../owallet/crates/owallet-web/tests/mock-overpay/server.mjs")],
  { env: { ...process.env, PORT: "0", MOCK_STREAM_POLLS: "2" }, stdio: ["ignore", "pipe", "inherit"] },
)
const overpay = await new Promise((resolve, reject) => {
  createInterface({ input: mock.stdout }).once("line", (line) => resolve(line.trim().split(" ").at(-1)))
  mock.on("exit", (code) => reject(new Error(`mock overpay exited (${code})`)))
})

const server = spawn(path.join(root, "node_modules/.bin/vite"), ["preview", "--port", String(port), "--strictPort"], {
  cwd: root,
  stdio: ["ignore", "pipe", "inherit"],
})
await new Promise((resolve, reject) => {
  server.stdout.on("data", (chunk) => String(chunk).includes(String(port)) && resolve())
  server.on("exit", (code) => reject(new Error(`vite preview exited (${code})`)))
})

const browser = await playwright.chromium.launch({ executablePath, args: ["--no-sandbox"] })
const results = []
const step = async (name, fn) => {
  const started = Date.now()
  console.log(`…    ${name}`)
  try {
    await fn()
    results.push(`ok   ${name} (${Date.now() - started} ms)`)
  } catch (error) {
    results.push(`FAIL ${name}: ${error.message}`)
    throw error
  }
}

try {
  const context = await browser.newContext({ viewport: { width: 1100, height: 900 } })
  const page = await context.newPage()
  page.on("pageerror", (error) => console.error("[pageerror]", error.message))
  page.on("console", (message) => message.type() === "error" && console.error("[console]", message.text()))
  // `?overpay=` is what the page passes as WorkerOptions.overpay once main.ts
  // reads it; the bridge's configure below points the worker at the mock
  // either way.
  const open = async () => {
    await page.goto(`${base}/?debug-panel&overpay=${encodeURIComponent(overpay)}`)
    await page.waitForFunction(() => document.querySelector("#status")?.dataset.state !== "booting", null, {
      timeout: 120_000,
    })
    assert.match(await page.textContent("#status"), /Core ready/)
    // Point the worker's owallet at the mock (src/owallet.ts configureOwallet,
    // inline), and reach owallet.internal through the worker's router
    // (the `privateFetch` RPC the TUI uses).
    await page.evaluate((overpay) => {
      const { core } = globalThis.norm
      globalThis.owallet = async (route, init = {}) => {
        const result = await core.client.call("privateFetch", {
          url: `http://owallet.internal${route}`,
          method: init.method ?? "GET",
          headers: { "content-type": "application/json", ...init.headers },
          body: init.body,
        })
        let body
        try {
          body = JSON.parse(result.body)
        } catch {
          body = result.body
        }
        return { status: result.status, body }
      }
      const id = `test-${Date.now()}`
      return new Promise((resolve) => {
        const onMessage = (event) => {
          if (typeof event.data !== "string" || !event.data.includes(id)) return
          core.worker.removeEventListener("message", onMessage)
          resolve()
        }
        core.worker.addEventListener("message", onMessage)
        core.worker.postMessage(
          JSON.stringify({ type: "owallet.configure", id, overpay: { railsUrl: overpay, env: "dev" } }),
        )
      })
    }, overpay)
  }
  const api = (route, init) => page.evaluate(([route, init]) => globalThis.norm.api(route, init), [route, init])
  const wallet = (route, init) => page.evaluate(([route, init]) => globalThis.owallet(route, init), [route, init])
  const post = (route, body) => wallet(route, { method: "POST", body: JSON.stringify(body ?? {}) })

  let key
  let npub
  await step("owallet-web answers through the worker: init, generate, register with Overpay", async () => {
    await open()
    const health = await wallet("/health")
    assert.equal(health.status, 200)
    assert.equal(health.body.name, "owallet")
    let status = await wallet("/_mgmt/status")
    assert.deepEqual([status.body.initialized, status.body.unlocked], [false, false])
    assert.equal(status.body.rails_url, overpay)
    assert.equal((await post("/_mgmt/init", { password: PASSWORD })).status, 200)
    const generated = await post("/_mgmt/generate")
    assert.equal(generated.status, 200, JSON.stringify(generated.body))
    npub = generated.body.npub
    assert.match(npub, /^npub1/)
    const linked = await post("/_mgmt/overpay/register")
    assert.equal(linked.status, 200, JSON.stringify(linked.body))
    status = await wallet("/_mgmt/status")
    assert.deepEqual(
      [status.body.initialized, status.body.unlocked, status.body.wallet?.npub, status.body.overpay_linked],
      [true, true, npub, true],
    )
  })

  await step("norm's bootstrap mints an owk_ key into auth.json", async () => {
    // Re-bootstrap the instance (the setup screen does this once the wallet is ready).
    await page.evaluate(() => globalThis.norm.core.client.call("reload", undefined))
    await api("/session")
    // The file API stays inside the project, so read norm's auth store
    // (/norm/data/auth.json) through what the core loaded from it: no config
    // sets a key, so the overpay provider's key is auth.json's.
    const providers = await api("/config/providers")
    key = providers.providers.find((p) => p.id === "overpay")?.key
    assert.match(key ?? "", /^owk_[0-9a-f]{64}$/, "auth.json holds norm's provider key")
  })

  await step("GET /v1/models lists the marketplace's models", async () => {
    const models = await wallet("/v1/models", { headers: { authorization: `Bearer ${key}` } })
    assert.equal(models.status, 200, JSON.stringify(models.body))
    assert.deepEqual(
      models.body.data.map((model) => model.id),
      ["default", "mock/chat-small", "mock/chat-large"],
    )
    const providers = await api("/config/providers")
    const overpayProvider = providers.providers.find((p) => p.id === "overpay")
    assert.ok(overpayProvider.models["mock/chat-small"], "the core's provider offers the marketplace models")
  })

  await step("a prompt streams the mock seller's reply; its cost is charged_cents", async () => {
    const session = await api("/session", { method: "POST", body: JSON.stringify({ title: "owallet-web" }) })
    await api(`/session/${session.id}/message`, {
      method: "POST",
      body: JSON.stringify({
        model: { providerID: "overpay", modelID: "mock/chat-small" },
        parts: [{ type: "text", text: "say hello" }],
      }),
    })
    const messages = await api(`/session/${session.id}/message`)
    const last = messages.at(-1)
    assert.equal(last.info.role, "assistant")
    assert.equal(last.info.error, undefined, JSON.stringify(last.info.error))
    const text = last.parts.filter((part) => part.type === "text").map((part) => part.text).join("")
    assert.equal(text.trim(), "Hello from the mock seller.")
    assert.equal(last.info.cost, 0.03, "the mock captured 3¢ (charged_cents)")
    await page.click("#refresh")
    await page.waitForSelector(`#sessions li[data-id="${session.id}"]`)
    await page.screenshot({ path: path.join(shots, "04-owallet-web-chat.png"), fullPage: true })
  })

  await step("the owallet MCP server connects and lists its tools", async () => {
    const status = await api("/mcp")
    assert.equal(status.owallet?.status, "connected", JSON.stringify(status))
    // The core's MCP client connected with norm's key (connecting lists the
    // server's tools); ask the server directly for the same list.
    const listed = await wallet("/mcp", {
      method: "POST",
      headers: { authorization: `Bearer ${key}`, accept: "application/json, text/event-stream" },
      body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/list", params: {} }),
    })
    assert.equal(listed.status, 200, JSON.stringify(listed.body))
    const names = listed.body.result.tools.map((tool) => tool.name)
    for (const name of ["get_account_info", "list_marketplace", "run_python"]) {
      assert.ok(names.includes(name), `${name} in ${names}`)
    }
  })

  await step("after a reload the wallet is there but locked; unlock works", async () => {
    await page.reload()
    await open()
    let status = await wallet("/_mgmt/status")
    assert.deepEqual(
      [status.body.initialized, status.body.unlocked, status.body.wallet?.npub],
      [true, false, npub],
      "the database persisted (OPFS); the password did not",
    )
    const wrong = await post("/_mgmt/unlock", { password: "nope" })
    assert.deepEqual([wrong.status, wrong.body.error?.code], [401, "bad_password"])
    const unlocked = await post("/_mgmt/unlock", { password: PASSWORD })
    assert.equal(unlocked.status, 200, JSON.stringify(unlocked.body))
    status = await wallet("/_mgmt/status")
    assert.deepEqual([status.body.unlocked, status.body.overpay_linked], [true, true])
    await page.screenshot({ path: path.join(shots, "05-owallet-web-unlocked.png"), fullPage: true })
  })
} finally {
  console.log(results.join("\n"))
  await browser.close()
  server.kill()
  mock.kill()
}
