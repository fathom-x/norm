// The visitor's session: an HMAC-SHA256-signed random `sid` in a cookie. No
// database — the sid is all the broker keeps, and the provider finds the
// visitor's sandbox by it (E2B: sandbox metadata). Without SESSION_SECRET a
// forged cookie could name someone else's sandbox, so verification is
// constant-time and anything malformed is simply a new visitor.
import { createHmac, randomBytes, timingSafeEqual } from "node:crypto"

export const COOKIE_NAME = "sid"
/** How long a visitor keeps their sandbox across visits (180 days). */
export const COOKIE_MAX_AGE_S = 180 * 24 * 3600
/** A sid: 22 base64url characters (16 random bytes). */
export const SID_PATTERN = /^[A-Za-z0-9_-]{22}$/

export function newSid(): string {
  return randomBytes(16).toString("base64url")
}

function mac(secret: string, sid: string): Buffer {
  return createHmac("sha256", secret).update(`sid:${sid}`).digest()
}

/** `<sid>.<base64url HMAC>` */
export function sign(secret: string, sid: string): string {
  return `${sid}.${mac(secret, sid).toString("base64url")}`
}

/** The sid in a signed value, or undefined if it is malformed or forged. */
export function verify(secret: string, value: string | undefined): string | undefined {
  if (!value) return undefined
  const dot = value.indexOf(".")
  if (dot < 0) return undefined
  const sid = value.slice(0, dot)
  if (!SID_PATTERN.test(sid)) return undefined
  const given = Buffer.from(value.slice(dot + 1), "base64url")
  const expected = mac(secret, sid)
  if (given.length !== expected.length) return undefined
  return timingSafeEqual(given, expected) ? sid : undefined
}

/** The value of cookie `name` in a Cookie header. */
export function readCookie(header: string | null, name = COOKIE_NAME): string | undefined {
  if (!header) return undefined
  for (const part of header.split(";")) {
    const eq = part.indexOf("=")
    if (eq < 0) continue
    if (part.slice(0, eq).trim() === name) return part.slice(eq + 1).trim()
  }
  return undefined
}

export function sessionFromRequest(secret: string, request: Request): string | undefined {
  return verify(secret, readCookie(request.headers.get("cookie")))
}

export function setCookieHeader(secret: string, sid: string, secure: boolean): string {
  const attributes = [
    `${COOKIE_NAME}=${sign(secret, sid)}`,
    "Path=/",
    `Max-Age=${COOKIE_MAX_AGE_S}`,
    "HttpOnly",
    "SameSite=Lax",
  ]
  if (secure) attributes.push("Secure")
  return attributes.join("; ")
}
