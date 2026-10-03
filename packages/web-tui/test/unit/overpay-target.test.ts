import { describe, expect, test } from "bun:test"
import { allowedOverpay, KNOWN_OVERPAY } from "../../src/overpay-target"

const site = { hostname: "norm.example" }
const local = { hostname: "127.0.0.1" }

describe("?overpay= allowlist", () => {
  test("the known deployments are allowed, as their origin", () => {
    for (const url of KNOWN_OVERPAY) expect(allowedOverpay(url, site)).toBe(url)
    expect(allowedOverpay("https://overpay.com/some/path", site)).toBe("https://overpay.com")
  })

  test("anything else is ignored", () => {
    for (const url of [
      "https://evil.example",
      "https://overpay.com.evil.example",
      "http://overpay.com",
      "https://overpay.com@evil.example",
      "https://user:pw@overpay.com",
      "javascript:alert(1)",
      "not a url",
    ])
      expect(allowedOverpay(url, site)).toBeUndefined()
  })

  test("loopback only from a loopback page (development and tests)", () => {
    expect(allowedOverpay("http://127.0.0.1:4010", local)).toBe("http://127.0.0.1:4010")
    expect(allowedOverpay("http://localhost:3001", { hostname: "localhost" })).toBe("http://localhost:3001")
    expect(allowedOverpay("http://127.0.0.1:4010", site)).toBeUndefined()
  })

  test("deployments listed at build time are allowed", () => {
    expect(allowedOverpay("https://overpay.staging.example", site, ["https://overpay.staging.example/"])).toBe(
      "https://overpay.staging.example",
    )
  })

  test("no parameter, no override", () => {
    expect(allowedOverpay(null, site)).toBeUndefined()
  })
})
