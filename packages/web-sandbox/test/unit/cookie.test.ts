import { describe, expect, test } from "bun:test"
import { newSid, readCookie, setCookieHeader, sign, SID_PATTERN, verify } from "../../server/cookie"

const SECRET = "x".repeat(40)

describe("session cookie", () => {
  test("a signed sid verifies", () => {
    const sid = newSid()
    expect(sid).toMatch(SID_PATTERN)
    expect(verify(SECRET, sign(SECRET, sid))).toBe(sid)
  })

  test("sids are random", () => {
    expect(newSid()).not.toBe(newSid())
  })

  test("a tampered sid or signature is refused", () => {
    const sid = newSid()
    const value = sign(SECRET, sid)
    const [, signature] = value.split(".")
    const otherSid = sid.slice(0, -1) + (sid.endsWith("A") ? "B" : "A")
    expect(verify(SECRET, `${otherSid}.${signature}`)).toBeUndefined()
    const flipped = signature.slice(0, -2) + (signature.at(-2) === "A" ? "B" : "A") + signature.at(-1)
    expect(verify(SECRET, `${sid}.${flipped}`)).toBeUndefined()
    expect(verify(SECRET, `${sid}.${signature}x`)).toBeUndefined()
    expect(verify(SECRET, `${sid}.`)).toBeUndefined()
  })

  test("another secret's signature is refused", () => {
    const sid = newSid()
    expect(verify("y".repeat(40), sign(SECRET, sid))).toBeUndefined()
  })

  test("malformed values are refused", () => {
    for (const value of [undefined, "", "abc", ".", "../../etc.passwd", `${"a".repeat(21)}.sig`, "a b.c"])
      expect(verify(SECRET, value)).toBeUndefined()
  })

  test("readCookie finds the sid among other cookies", () => {
    expect(readCookie("a=1; sid=abc.def; b=2")).toBe("abc.def")
    expect(readCookie("sidx=1; xsid=2")).toBeUndefined()
    expect(readCookie(null)).toBeUndefined()
  })

  test("Set-Cookie attributes", () => {
    const header = setCookieHeader(SECRET, newSid(), false)
    expect(header).toContain("HttpOnly")
    expect(header).toContain("SameSite=Lax")
    expect(header).toContain("Path=/")
    expect(header).toMatch(/Max-Age=\d{7,}/)
    expect(header).not.toContain("Secure")
    expect(setCookieHeader(SECRET, newSid(), true)).toContain("; Secure")
  })
})
