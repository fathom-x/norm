// Who is asking, and from where: the request's own origin, the client IP, and
// the same-origin check that keeps another site from opening a visitor's
// terminal (cross-site WebSocket hijacking: the browser would attach the
// SameSite=Lax cookie to a WebSocket handshake started by any page) or
// resetting their sandbox.

export interface RequestContext {
  /** Proxy hops in front of this server whose X-Forwarded-* to trust (0 = none). */
  trustProxy: number
  allowedOrigins: string[]
}

function first(header: string | null): string | undefined {
  return header?.split(",")[0]?.trim() || undefined
}

/** True when the request reached us over https (directly or, trusted, via the proxy). */
export function isHttps(request: Request, context: Pick<RequestContext, "trustProxy">): boolean {
  if (context.trustProxy) {
    const proto = first(request.headers.get("x-forwarded-proto"))
    if (proto) return proto === "https"
  }
  return new URL(request.url).protocol === "https:"
}

/** This server's origin as the client addressed it (Host header + scheme). */
export function selfOrigin(request: Request, context: Pick<RequestContext, "trustProxy">): string {
  const url = new URL(request.url)
  const host = (context.trustProxy && first(request.headers.get("x-forwarded-host"))) || request.headers.get("host") || url.host
  return `${isHttps(request, context) ? "https" : "http"}://${host}`.toLowerCase()
}

/**
 * The request comes from one of our own pages: its Origin is this server's
 * origin or listed in ALLOWED_ORIGINS. A missing Origin is refused — every
 * browser sends one on WebSocket handshakes and POSTs.
 */
export function originAllowed(request: Request, context: RequestContext): boolean {
  const origin = request.headers.get("origin")
  if (!origin || origin === "null") return false
  let normalized: string
  try {
    normalized = new URL(origin).origin.toLowerCase()
  } catch {
    return false
  }
  if (normalized === selfOrigin(request, context)) return true
  return context.allowedOrigins.some((allowed) => allowed.toLowerCase() === normalized)
}

/**
 * The client IP: the socket's, or behind `trustProxy` proxies the address the
 * outermost of them saw. Counted from the right of X-Forwarded-For: each proxy
 * appends the peer it saw, and anything further left came from the client and
 * could be made up.
 */
export function clientIp(request: Request, socketIp: string | undefined, context: Pick<RequestContext, "trustProxy">) {
  if (context.trustProxy > 0) {
    const hops = (request.headers.get("x-forwarded-for") ?? "")
      .split(",")
      .map((hop) => hop.trim())
      .filter(Boolean)
    const ip = hops[hops.length - context.trustProxy]
    if (ip) return ip
  }
  return socketIp ?? "unknown"
}
