// Browser smoke test for the core worker (Spike 2 exit criteria), through the
// debug panel (`?debug-panel`), against the
// built page (`vite build` first; `bun run test:browser` does both).
//
//   node test/browser/smoke.mjs
//
// Env: CHROME (Chromium binary), PLAYWRIGHT (path to playwright's index.js),
// PORT (preview port, default 4317), DIST (the build to serve, default dist/).
// Screenshots land in test/screenshots/.
import assert from "node:assert/strict"
import { spawn } from "node:child_process"
import { mkdirSync } from "node:fs"
import { createRequire } from "node:module"
import path from "node:path"
import { fileURLToPath } from "node:url"

const root = path.resolve(fileURLToPath(new URL("../..", import.meta.url)))
const shots = path.join(root, "test/screenshots")
const port = Number(process.env.PORT ?? 4317)
const base = `http://127.0.0.1:${port}`
const playwright = createRequire(import.meta.url)(
  process.env.PLAYWRIGHT ?? "/opt/node22/lib/node_modules/playwright/index.js",
)
const executablePath = process.env.CHROME ?? "/opt/pw-browsers/chromium-1194/chrome-linux/chrome"

mkdirSync(shots, { recursive: true })
// DIST: the build to serve (default dist/; `vite build --outDir <dir>` elsewhere
// keeps parallel builds from overwriting each other).
const dist = process.env.DIST ?? path.join(root, "dist")
const server = spawn(
  path.join(root, "node_modules/.bin/vite"),
  ["preview", "--port", String(port), "--strictPort", "--outDir", dist],
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
  // One context = one origin-private file system, kept across reloads.
  const context = await browser.newContext({ viewport: { width: 1100, height: 900 } })
  const page = await context.newPage()
  page.on("pageerror", (error) => console.error("[pageerror]", error.message))
  page.on("console", (message) => message.type() === "error" && console.error("[console]", message.text()))
  const ready = async () => {
    await page.waitForFunction(() => document.querySelector("#status")?.dataset.state !== "booting", null, {
      timeout: 120_000,
    })
    const status = await page.textContent("#status")
    assert.match(status, /Core ready/, status)
    return status
  }
  const api = (route, init) =>
    page.evaluate(([route, init]) => globalThis.norm.api(route, init), [route, init])

  let created
  await step("boots the core worker on OPFS and seeds the demo workspace", async () => {
    await page.goto(`${base}/?debug-panel`)
    const status = await ready()
    assert.match(status, /files: opfs \(demo workspace created\)/)
  })

  await step("POST /session then GET /session round-trips through Rpc", async () => {
    created = await api("/session", { method: "POST", body: JSON.stringify({ title: "browser smoke" }) })
    assert.equal(created.directory, "/workspace")
    const sessions = await api("/session")
    assert.ok(sessions.some((session) => session.id === created.id), "created session is listed")
    await page.click("#refresh")
    await page.waitForSelector(`#sessions li[data-id="${created.id}"]`)
    await page.screenshot({ path: path.join(shots, "01-session-created.png"), fullPage: true })
  })

  await step("reads the demo workspace through the file API", async () => {
    const file = await api("/file/content?path=README.md")
    assert.match(file.content, /# Tip calculator/)
    await page.fill("#path", "src/tip.ts")
    await page.click("#read")
    await page.waitForFunction(() => document.querySelector("#content")?.textContent?.includes("splitBill"))
  })

  await step("sessions and files survive a reload (OPFS)", async () => {
    await page.reload()
    const status = await ready()
    assert.doesNotMatch(status, /demo workspace created/, "the workspace is not re-seeded")
    const sessions = await api("/session")
    assert.ok(sessions.some((session) => session.id === created.id), "session still listed after reload")
    await page.waitForSelector(`#sessions li[data-id="${created.id}"]`)
    await page.screenshot({ path: path.join(shots, "02-after-reload.png"), fullPage: true })
  })

  // The same tab, now with the scripted owallet answering owallet.internal:
  // a prompt drives the real session loop through the browser tool set.
  const prompt = async (text) => {
    const session = await api("/session", { method: "POST", body: JSON.stringify({ title: text }) })
    await api(`/session/${session.id}/message`, {
      method: "POST",
      body: JSON.stringify({ model: { providerID: "overpay", modelID: "default" }, parts: [{ type: "text", text }] }),
    })
    const messages = await api(`/session/${session.id}/message`)
    const parts = messages.flatMap((message) => message.parts)
    return { tools: parts.filter((part) => part.type === "tool"), messages }
  }

  await step("write + read tool round trip on the workspace (scripted model)", async () => {
    await page.goto(`${base}/?debug-panel&mock-owallet`)
    await ready()
    const { tools, messages } = await prompt("write a note, then read it back")
    assert.deepEqual(
      tools.map((part) => [part.tool, part.state.status]),
      [
        ["write", "completed"],
        ["read", "completed"],
      ],
    )
    assert.match(tools[1].state.output, /hello from the scripted model/)
    const file = await api("/file/content?path=notes/hello.md")
    assert.equal(file.content.trim(), "hello from the scripted model")
    const last = messages.at(-1)
    assert.equal(last.info.cost, 0.01, "owallet's charged_cents is the turn's cost")
    await page.click("#refresh")
    await page.fill("#path", "notes/hello.md")
    await page.click("#read")
    await page.waitForFunction(() => document.querySelector("#content")?.textContent?.includes("scripted model"))
    await page.screenshot({ path: path.join(shots, "03-tool-round-trip.png"), fullPage: true })
  })

  await step("grep and glob run in JS over the VFS", async () => {
    const { tools } = await prompt("search the project")
    assert.deepEqual(
      tools.map((part) => [part.tool, part.state.status]),
      [
        ["grep", "completed"],
        ["glob", "completed"],
      ],
    )
    assert.match(tools[0].state.output, /src\/tip\.ts/)
    assert.match(tools[0].state.output, /src\/format\.ts/)
    assert.match(tools[1].state.output, /src\/main\.ts/)
  })

  await step("read + edit a workspace file", async () => {
    const { tools } = await prompt("edit the formatter")
    assert.deepEqual(
      tools.map((part) => [part.tool, part.state.status]),
      [
        ["read", "completed"],
        ["edit", "completed"],
      ],
    )
    const file = await api("/file/content?path=src/format.ts")
    assert.match(file.content, /Formats US dollars only/)
    assert.doesNotMatch(file.content, /TODO/)
  })

  await step("bash refuses with the browser-build message", async () => {
    const { tools } = await prompt("try bash")
    assert.equal(tools[0].tool, "bash")
    assert.match(tools[0].state.output, /not available in the browser build/)
    assert.match(tools[0].state.output, /run_python/)
  })
} finally {
  console.log(results.join("\n"))
  await browser.close()
  server.kill()
}
