// Spike 0 exit criteria for opentui-on-wasm, checked in headless Chromium.
//
//   node test/demo.test.mjs            (after `npm run build` in demo/)
//
// Serves demo/dist, loads the core and the solid scenes, and asserts by
// reading xterm.js's buffer. Screenshots go to test/screenshots/.
import http from "node:http"
import fs from "node:fs"
import path from "node:path"
import { fileURLToPath } from "node:url"
import pkg from "/opt/node22/lib/node_modules/playwright/index.js"
const { chromium } = pkg

const here = path.dirname(fileURLToPath(import.meta.url))
const distDir = path.resolve(here, "../demo/dist")
const shotsDir = path.join(here, "screenshots")
fs.mkdirSync(shotsDir, { recursive: true })

const CHROMIUM =
  process.env.CHROMIUM_PATH ??
  ["/opt/pw-browsers/chromium-1194/chrome-linux/chrome", "/opt/pw-browsers/chromium/chrome-linux/chrome"].find((p) =>
    fs.existsSync(p),
  )

const MIME = {
  ".html": "text/html",
  ".js": "text/javascript",
  ".css": "text/css",
  ".wasm": "application/wasm",
  ".map": "application/json",
  ".ts": "text/plain",
}

function serve(root) {
  const server = http.createServer((req, res) => {
    const url = new URL(req.url, "http://x")
    let file = path.join(root, decodeURIComponent(url.pathname))
    if (!file.startsWith(root)) return res.writeHead(403).end()
    if (fs.existsSync(file) && fs.statSync(file).isDirectory()) file = path.join(file, "index.html")
    if (!fs.existsSync(file)) return res.writeHead(404).end("not found")
    res.writeHead(200, { "content-type": MIME[path.extname(file)] ?? "application/octet-stream" })
    fs.createReadStream(file).pipe(res)
  })
  return new Promise((resolve) => server.listen(0, "127.0.0.1", () => resolve(server)))
}

const results = []
function check(name, ok, detail = "") {
  results.push({ name, ok, detail })
  console.log(`${ok ? "PASS" : "FAIL"}  ${name}${detail ? `  — ${detail}` : ""}`)
}

async function screen(page) {
  return page.evaluate(() => {
    const buf = window.term.buffer.active
    const lines = []
    for (let i = 0; i < window.term.rows; i++) lines.push(buf.getLine(buf.viewportY + i)?.translateToString(true) ?? "")
    return lines
  })
}

async function waitForScreen(page, predicate, timeout = 5000) {
  const start = Date.now()
  let lines = await screen(page)
  while (!predicate(lines)) {
    if (Date.now() - start > timeout) return { ok: false, lines }
    await page.waitForTimeout(50)
    lines = await screen(page)
  }
  return { ok: true, lines }
}

const boxWidth = (lines) => {
  const top = lines.find((l) => l.includes("╭"))
  if (!top) return -1
  return top.lastIndexOf("╮") - top.indexOf("╭") + 1
}

const server = await serve(distDir)
const base = `http://127.0.0.1:${server.address().port}/`
const browser = await chromium.launch({ executablePath: CHROMIUM, args: ["--no-sandbox"] })
let failed = false
try {
  // ---- core scene -------------------------------------------------------
  const page = await browser.newPage({ viewport: { width: 1100, height: 700 } })
  const errors = []
  page.on("pageerror", (e) => errors.push(String(e)))
  page.on("console", (m) => {
    if (m.type() === "error") errors.push(`${m.text()} ${m.location()?.url ?? ""}`)
  })
  page.on("requestfailed", (req) => errors.push(`request failed: ${req.url()}`))
  page.on("response", (res) => {
    if (res.status() >= 400 && !res.url().endsWith("/favicon.ico")) errors.push(`HTTP ${res.status()} ${res.url()}`)
  })
  await page.goto(`${base}?scene=core&cols=80&rows=24`)
  await page.waitForFunction(() => window.demoReady === true, null, { timeout: 20000 })

  let r = await waitForScreen(page, (l) => l.some((x) => x.includes("hello from wasm")))
  check("core: TextRenderable 'hello from wasm' in xterm buffer", r.ok)
  check("core: Box border + title", r.lines.some((l) => l.includes("╭") && l.includes("opentui · wasm")))
  check("core: InputRenderable placeholder", r.lines.some((l) => l.includes("type here")))
  const width80 = boxWidth(r.lines)
  check("core: box spans 80 columns", width80 === 80, `width=${width80}`)
  await page.screenshot({ path: path.join(shotsDir, "01-core-80x24.png") })
  console.log(r.lines.slice(0, 9).join("\n"))

  // resize
  await page.evaluate(() => window.term.resize(100, 30))
  r = await waitForScreen(page, (l) => boxWidth(l) === 100)
  check("core: resize to 100x30 re-renders box at 100 columns", r.ok, `width=${boxWidth(r.lines)}`)
  await page.screenshot({ path: path.join(shotsDir, "02-core-100x30.png") })

  // typing
  await page.click("#terminal")
  await page.keyboard.type("abc")
  r = await waitForScreen(page, (l) => l.some((x) => x.includes("abc")))
  check("core: typed 'abc' reaches the focused input", r.ok)
  const value = await page.evaluate(() => window.demo.getInputValue())
  check("core: InputRenderable.value === 'abc'", value === "abc", JSON.stringify(value))
  await page.screenshot({ path: path.join(shotsDir, "03-core-typed.png") })

  // frame time
  const frames = await page.evaluate(() => window.demo.measureFrames(1500))
  console.log(`frame time at 100x30 (renderer stats, text changing every frame): ${JSON.stringify(frames)}`)
  check("core: frames rendered while measuring", frames.frames > 10, `${frames.frames} frames`)
  const realErrors = errors.filter((e) => !e.includes("favicon.ico") && !e.startsWith("Failed to load resource"))
  check("core: no page errors", realErrors.length === 0, realErrors.slice(0, 3).join(" | "))
  await page.close()

  // ---- solid scene ------------------------------------------------------
  const solidPage = await browser.newPage({ viewport: { width: 1100, height: 700 } })
  const solidErrors = []
  solidPage.on("pageerror", (e) => solidErrors.push(String(e)))
  await solidPage.goto(`${base}?scene=solid&cols=80&rows=24`)
  await solidPage.waitForFunction(() => window.demoReady === true, null, { timeout: 20000 })
  r = await waitForScreen(solidPage, (l) => l.some((x) => x.includes("hello from solid on wasm")))
  check("solid: render() shows box + text", r.ok && r.lines.some((l) => l.includes("┌") && l.includes("solid")))
  const tick1 = r.lines.find((l) => l.includes("ticks:"))
  r = await waitForScreen(solidPage, (l) => {
    const t = l.find((x) => x.includes("ticks:"))
    return t !== undefined && t !== tick1
  })
  check("solid: signal updates re-render", r.ok, r.lines.find((l) => l.includes("ticks:"))?.trim())
  check("solid: no page errors", solidErrors.length === 0, solidErrors.slice(0, 3).join(" | "))
  await solidPage.screenshot({ path: path.join(shotsDir, "04-solid.png") })
  console.log(r.lines.slice(0, 7).join("\n"))
  await solidPage.close()
} catch (error) {
  failed = true
  console.error(error)
} finally {
  await browser.close()
  server.close()
}

const failures = results.filter((r) => !r.ok)
console.log(`\n${results.length - failures.length}/${results.length} checks passed`)
process.exit(failed || failures.length > 0 ? 1 : 0)
