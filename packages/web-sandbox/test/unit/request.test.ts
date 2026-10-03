import { describe, expect, test } from "bun:test"
import { clientIp, isHttps, originAllowed, selfOrigin } from "../../server/request"

const req = (url: string, headers: Record<string, string> = {}) => new Request(url, { headers })
const direct = { trustProxy: 0, allowedOrigins: [] }

describe("origin check", () => {
  test("same origin is allowed", () => {
    expect(originAllowed(req("http://127.0.0.1:4330/api/tty", { origin: "http://127.0.0.1:4330" }), direct)).toBe(true)
  })

  test("another site, another port, another scheme are refused", () => {
    for (const origin of ["https://evil.example", "http://127.0.0.1:4331", "https://127.0.0.1:4330", "null"])
      expect(originAllowed(req("http://127.0.0.1:4330/api/tty", { origin }), direct)).toBe(false)
  })

  test("a missing Origin is refused", () => {
    expect(originAllowed(req("http://127.0.0.1:4330/api/tty"), direct)).toBe(false)
  })

  test("ALLOWED_ORIGINS adds origins", () => {
    const context = { trustProxy: 0, allowedOrigins: ["https://demo.example"] }
    expect(originAllowed(req("http://10.0.0.5:4330/api/tty", { origin: "https://demo.example" }), context)).toBe(true)
    expect(originAllowed(req("http://10.0.0.5:4330/api/tty", { origin: "https://demo.example.evil" }), context)).toBe(
      false,
    )
  })

  test("behind a trusted proxy the forwarded scheme and host count", () => {
    const request = req("http://10.0.0.5:4330/api/tty", {
      host: "10.0.0.5:4330",
      "x-forwarded-proto": "https",
      "x-forwarded-host": "demo.example",
      origin: "https://demo.example",
    })
    expect(selfOrigin(request, { trustProxy: 1 })).toBe("https://demo.example")
    expect(originAllowed(request, { trustProxy: 1, allowedOrigins: [] })).toBe(true)
    // Not trusted: the forwarded headers are ignored.
    expect(originAllowed(request, direct)).toBe(false)
    expect(isHttps(request, { trustProxy: 0 })).toBe(false)
    expect(isHttps(request, { trustProxy: 1 })).toBe(true)
  })
})

describe("client IP", () => {
  test("the socket address unless a proxy is trusted", () => {
    const request = req("http://x/", { "x-forwarded-for": "6.6.6.6, 1.2.3.4" })
    expect(clientIp(request, "10.0.0.1", { trustProxy: 0 })).toBe("10.0.0.1")
  })

  test("behind N proxies, the Nth address from the right (the client cannot spoof it)", () => {
    const request = req("http://x/", { "x-forwarded-for": "6.6.6.6, 1.2.3.4" })
    expect(clientIp(request, "10.0.0.1", { trustProxy: 1 })).toBe("1.2.3.4")
    expect(clientIp(request, "10.0.0.1", { trustProxy: 2 })).toBe("6.6.6.6")
    expect(clientIp(req("http://x/"), "10.0.0.1", { trustProxy: 1 })).toBe("10.0.0.1")
  })
})
