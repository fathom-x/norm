// The HTTP + WebSocket surface, with a fake provider, on a random port.
import { afterAll, beforeAll, describe, expect, test } from "bun:test"
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import path from "node:path"
import { loadConfig } from "../../server/config"
import { COOKIE_NAME, newSid, sign } from "../../server/cookie"
import { CLOSE } from "../../server/hub"
import { createServer, type DemoServer } from "../../server/server"
import { FakeProvider, until } from "./fakes"

const SECRET = "t".repeat(40)
let dir: string
let demo: DemoServer
let provider: FakeProvider
let base: string
let origin: string

beforeAll(() => {
  dir = mkdtempSync(path.join(tmpdir(), "web-sandbox-server-"))
  const web = path.join(dir, "web")
  const browser = path.join(dir, "browser")
  mkdirSync(path.join(web, "sandbox"), { recursive: true })
  mkdirSync(path.join(web, "assets"), { recursive: true })
  mkdirSync(path.join(browser, "assets"), { recursive: true })
  writeFileSync(path.join(web, "index.html"), "<h1>landing</h1>")
  writeFileSync(path.join(web, "sandbox/index.html"), "<h1>sandbox</h1>")
  writeFileSync(path.join(web, "assets/sandbox-AbCd1234.js"), "console.log(1)")
  writeFileSync(path.join(dir, "secret.txt"), "outside the web root")
  writeFileSync(path.join(browser, "index.html"), "<h1>browser</h1>")
  writeFileSync(path.join(browser, "assets/opentui-Zz9_8765.wasm"), new Uint8Array([0, 97, 115, 109]))
  const config = loadConfig({
    PORT: "0",
    SESSION_SECRET: SECRET,
    WEB_DIST: web,
    BROWSER_DIST: browser,
    PAUSE_GRACE_MS: "30",
  })
  provider = new FakeProvider()
  demo = createServer(config, { provider, log: () => {} })
  base = demo.url.href.replace(/\/$/, "")
  origin = base
})

afterAll(async () => {
  await demo.stop()
  rmSync(dir, { recursive: true, force: true })
})

const cookieFor = (sid: string) => `${COOKIE_NAME}=${sign(SECRET, sid)}`

function openSocket(headers: Record<string, string>) {
  const ws = new WebSocket(`${base.replace("http", "ws")}/api/tty`, { headers } as unknown as string[])
  ws.binaryType = "arraybuffer"
  const texts: Array<{ phase: string; message?: string; code?: number }> = []
  const binaries: string[] = []
  let closed: { code: number } | undefined
  ws.onmessage = (event) => {
    if (typeof event.data === "string") texts.push(JSON.parse(event.data))
    else binaries.push(new TextDecoder().decode(event.data as ArrayBuffer))
  }
  ws.onclose = (event) => (closed = { code: event.code })
  return {
    ws,
    texts,
    binaries,
    get closed() {
      return closed
    },
    opened: new Promise<void>((resolve, reject) => {
      ws.onopen = () => resolve()
      ws.onerror = () => reject(new Error("socket error"))
    }),
  }
}

describe("http", () => {
  test("healthz", async () => {
    const response = await fetch(`${base}/healthz`)
    expect(await response.json()).toMatchObject({ ok: true, provider: "fake" })
  })

  test("/api/session issues a signed cookie once and reports state without the sid", async () => {
    const first = await fetch(`${base}/api/session`)
    const setCookie = first.headers.get("set-cookie")!
    expect(setCookie).toMatch(/^sid=[A-Za-z0-9_-]{22}\.[A-Za-z0-9_-]+; Path=\/; Max-Age=\d+; HttpOnly; SameSite=Lax$/)
    const body = await first.json()
    expect(body).toEqual({ exists: false, state: "none" })
    const cookie = setCookie.split(";")[0]
    const sid = cookie.slice(4).split(".")[0]
    provider.states.set(sid, "paused")
    const second = await fetch(`${base}/api/session`, { headers: { cookie } })
    expect(second.headers.get("set-cookie")).toBeNull()
    expect(await second.json()).toEqual({ exists: true, state: "paused" })
    // A forged cookie is a new visitor.
    const forged = await fetch(`${base}/api/session`, { headers: { cookie: `sid=${sid}.AAAA` } })
    expect(forged.headers.get("set-cookie")).not.toBeNull()
  })

  test("Secure cookie behind an https proxy only when the proxy is trusted", async () => {
    const response = await fetch(`${base}/sandbox/`, { headers: { "x-forwarded-proto": "https" } })
    expect(response.headers.get("set-cookie")).not.toContain("Secure")
  })

  test("pages, assets, caching and content types", async () => {
    const landing = await fetch(`${base}/`)
    expect(await landing.text()).toBe("<h1>landing</h1>")
    expect(landing.headers.get("content-type")).toBe("text/html; charset=utf-8")
    expect(landing.headers.get("cache-control")).toBe("no-cache")
    expect(landing.headers.get("x-frame-options")).toBe("DENY")
    expect(landing.headers.get("set-cookie")).toBeNull()

    const sandbox = await fetch(`${base}/sandbox/`)
    expect(await sandbox.text()).toBe("<h1>sandbox</h1>")
    expect(sandbox.headers.get("set-cookie")).toContain("sid=")

    const redirect = await fetch(`${base}/sandbox`, { redirect: "manual" })
    expect(redirect.status).toBe(301)
    expect(redirect.headers.get("location")).toBe("/sandbox/")

    const asset = await fetch(`${base}/assets/sandbox-AbCd1234.js`)
    expect(asset.headers.get("content-type")).toBe("text/javascript; charset=utf-8")
    expect(asset.headers.get("cache-control")).toContain("immutable")

    const wasm = await fetch(`${base}/browser/assets/opentui-Zz9_8765.wasm`)
    expect(wasm.headers.get("content-type")).toBe("application/wasm")
    expect(new Uint8Array(await wasm.arrayBuffer())).toEqual(new Uint8Array([0, 97, 115, 109]))
    expect(await (await fetch(`${base}/browser/`)).text()).toBe("<h1>browser</h1>")
    const head = await fetch(`${base}/browser/`, { method: "HEAD" })
    expect(head.status).toBe(200)

    expect(await (await fetch(`${base}/api/features`)).json()).toEqual({ browser: true, provider: "fake" })
  })

  test("no escaping the web root", async () => {
    for (const p of ["/../secret.txt", "/%2e%2e/secret.txt", "/assets/..%2f..%2fsecret.txt", "/browser/..%2fsecret.txt"]) {
      const response = await fetch(`${base}${p}`)
      // Either the client normalized the path (and got a page) or the server refused it.
      expect(await response.text()).not.toContain("outside the web root")
    }
    expect((await fetch(`${base}/assets/..%2f..%2fsecret.txt`)).status).toBe(404)
    expect((await fetch(`${base}/nope.html`)).status).toBe(404)
    expect((await fetch(`${base}/api/nope`)).status).toBe(404)
  })

  test("/api/reset needs the cookie, same origin and POST", async () => {
    const sid = newSid()
    provider.states.set(sid, "running")
    expect((await fetch(`${base}/api/reset`, { method: "POST", headers: { origin } })).status).toBe(401)
    expect(
      (await fetch(`${base}/api/reset`, { method: "POST", headers: { cookie: cookieFor(sid), origin: "https://evil.example" } }))
        .status,
    ).toBe(403)
    expect((await fetch(`${base}/api/reset`, { method: "POST", headers: { cookie: cookieFor(sid) } })).status).toBe(403)
    expect((await fetch(`${base}/api/reset`, { headers: { cookie: cookieFor(sid), origin } })).status).toBe(405)
    const ok = await fetch(`${base}/api/reset`, { method: "POST", headers: { cookie: cookieFor(sid), origin } })
    expect(ok.status).toBe(200)
    expect(await provider.status(sid)).toBe("none")
  })
})

describe("websocket", () => {
  test("refused without a cookie or from another origin", async () => {
    const noUpgrade = await fetch(`${base}/api/tty`)
    expect(noUpgrade.status).toBe(426)
    const handshake = (headers: Record<string, string>) =>
      fetch(`${base}/api/tty`, {
        headers: {
          upgrade: "websocket",
          connection: "Upgrade",
          "sec-websocket-version": "13",
          "sec-websocket-key": "dGhlIHNhbXBsZSBub25jZQ==",
          ...headers,
        },
      })
    expect((await handshake({ origin })).status).toBe(401)
    expect((await handshake({ origin: "https://evil.example", cookie: cookieFor(newSid()) })).status).toBe(403)
    expect((await handshake({ cookie: cookieFor(newSid()) })).status).toBe(403)
  })

  test("full session: hello, ready, bytes both ways, resize, takeover, grace pause", async () => {
    const sid = newSid()
    const headers = { cookie: cookieFor(sid), origin }
    const a = openSocket(headers)
    await a.opened
    a.ws.send(JSON.stringify({ type: "hello", cols: 80, rows: 24 }))
    await until(() => a.texts.some((t) => t.phase === "ready"), 2000, "ready")
    expect(a.texts.map((t) => t.phase)).toEqual(["creating", "starting", "ready"])

    const terminal = provider.terminal(sid)
    a.ws.send(new TextEncoder().encode("echo hi\r"))
    await until(() => terminal.input.length === 1, 2000, "input")
    expect(new TextDecoder().decode(terminal.input[0])).toBe("echo hi\r")
    terminal.output("hi\r\n")
    await until(() => a.binaries.join("") === "hi\r\n", 2000, "output")
    a.ws.send(JSON.stringify({ type: "resize", cols: 132, rows: 43 }))
    await until(() => terminal.sizes.at(-1)?.[0] === 132, 2000, "resize")

    // A second tab takes over.
    const b = openSocket(headers)
    await b.opened
    b.ws.send(JSON.stringify({ type: "hello", cols: 100, rows: 30 }))
    await until(() => b.texts.some((t) => t.phase === "ready"), 2000, "b ready")
    await until(() => a.closed !== undefined, 2000, "a closed")
    expect(a.texts.at(-1)?.phase).toBe("replaced")
    expect(a.closed?.code).toBe(CLOSE.replaced)
    expect(b.texts.map((t) => t.phase)).toEqual(["starting", "ready"])

    // Gone: paused after the grace period.
    b.ws.close()
    await until(() => provider.calls.includes(`pause ${sid}`), 2000, "pause")
  })

  test("oversized control frames close the socket", async () => {
    const sid = newSid()
    const s = openSocket({ cookie: cookieFor(sid), origin })
    await s.opened
    s.ws.send(JSON.stringify({ type: "hello", cols: 80, rows: 24, pad: "x".repeat(10_000) }))
    await until(() => s.closed !== undefined, 2000, "closed")
    expect(s.closed?.code).toBe(CLOSE.protocol)
  })
})
