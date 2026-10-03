// The demo service: Bun HTTP + native WebSocket.
//
//   GET  /healthz       liveness
//   GET  /api/features  {browser, provider}: what the landing page offers
//   GET  /api/session   {exists, state} of the visitor's sandbox (issues the cookie)
//   GET  /api/tty       WebSocket: the visitor's terminal (hub.ts has the protocol)
//   POST /api/reset     delete the visitor's sandbox ("Start over")
//   GET  /, /sandbox/   this package's pages (dist/web)
//   GET  /browser/…     variant A's build, when BROWSER_DIST is set
import type { Server, ServerWebSocket } from "bun"
import type { Config } from "./config"
import { newSid, sessionFromRequest, setCookieHeader } from "./cookie"
import { admission, Connection, Hub, LIMITS } from "./hub"
import type { SandboxProvider } from "./provider"
import { createProvider } from "./providers"
import { RateLimiter } from "./rate-limit"
import { clientIp, isHttps, originAllowed, type RequestContext } from "./request"
import { fileResponse, resolveFile } from "./static"

interface SocketData {
  sid: string
  ip: string
  connection?: Connection
}

export interface CreateServerOptions {
  /** Default: the one SANDBOX_PROVIDER names (providers/index.ts). */
  provider?: SandboxProvider
  log?: (message: string) => void
}

export interface DemoServer {
  server: Server<SocketData>
  hub: Hub
  url: URL
  stop(): Promise<void>
}

const SECURITY_HEADERS: Record<string, string> = {
  "x-content-type-options": "nosniff",
  "referrer-policy": "same-origin",
}

function json(body: unknown, init: ResponseInit = {}) {
  const headers = new Headers(init.headers)
  headers.set("content-type", "application/json")
  headers.set("cache-control", "no-store")
  return new Response(JSON.stringify(body), { ...init, headers })
}

function withSecurityHeaders(response: Response) {
  for (const [name, value] of Object.entries(SECURITY_HEADERS)) response.headers.set(name, value)
  if (response.headers.get("content-type")?.startsWith("text/html")) {
    // The terminal types into a real shell: never inside someone else's frame.
    response.headers.set("content-security-policy", "frame-ancestors 'none'")
    response.headers.set("x-frame-options", "DENY")
  }
  return response
}

export function createServer(config: Config, options: CreateServerOptions = {}): DemoServer {
  const log = options.log ?? ((message: string) => console.log(message))
  const provider = options.provider ?? createProvider(config, log)
  const context: RequestContext = { trustProxy: config.trustProxy, allowedOrigins: config.allowedOrigins }
  const limiter = new RateLimiter(config.createsPerIpPerHour, 3_600_000)
  const hub = new Hub({
    provider,
    pauseGraceMs: config.pauseGraceMs,
    helloTimeoutMs: config.helloTimeoutMs,
    admit: admission({ provider, maxSandboxes: config.maxSandboxes, limiter }),
    log,
  })

  const secure = (request: Request) => config.cookieSecure || isHttps(request, context)

  /** The visitor's sid, and the Set-Cookie header when it is new. */
  function session(request: Request): { sid: string; setCookie?: string } {
    const sid = sessionFromRequest(config.sessionSecret, request)
    if (sid) return { sid }
    const fresh = newSid()
    return { sid: fresh, setCookie: setCookieHeader(config.sessionSecret, fresh, secure(request)) }
  }

  function serveFrom(root: string, urlPath: string, request: Request, headers?: Headers) {
    const file = resolveFile(root, urlPath)
    return file ? fileResponse(file, urlPath, request.method, headers) : new Response("not found", { status: 404 })
  }

  async function handle(request: Request, server: Server<SocketData>): Promise<Response | undefined> {
    const url = new URL(request.url)
    const { pathname } = url
    const method = request.method

    if (pathname === "/healthz") return json({ ok: true, provider: provider.name, connections: hub.size })

    if (pathname === "/api/tty") {
      if (request.headers.get("upgrade")?.toLowerCase() !== "websocket")
        return json({ error: "websocket upgrade required" }, { status: 426 })
      if (!originAllowed(request, context)) return json({ error: "origin not allowed" }, { status: 403 })
      const sid = sessionFromRequest(config.sessionSecret, request)
      if (!sid) return json({ error: "no session" }, { status: 401 })
      const ip = clientIp(request, server.requestIP(request)?.address, context)
      if (server.upgrade(request, { data: { sid, ip } })) return undefined
      return json({ error: "upgrade failed" }, { status: 400 })
    }

    if (pathname === "/api/features") return json({ browser: !!config.browserDist, provider: provider.name })

    if (pathname === "/api/session") {
      if (method !== "GET") return json({ error: "method not allowed" }, { status: 405 })
      const { sid, setCookie } = session(request)
      const state = setCookie ? "none" : await provider.status(sid)
      const headers = setCookie ? { "set-cookie": setCookie } : undefined
      return json({ exists: state !== "none", state }, { headers })
    }

    if (pathname === "/api/reset") {
      if (method !== "POST") return json({ error: "method not allowed" }, { status: 405 })
      if (!originAllowed(request, context)) return json({ error: "origin not allowed" }, { status: 403 })
      const sid = sessionFromRequest(config.sessionSecret, request)
      if (!sid) return json({ error: "no session" }, { status: 401 })
      await hub.reset(sid)
      return json({ ok: true })
    }

    if (pathname.startsWith("/api/")) return json({ error: "not found" }, { status: 404 })
    if (method !== "GET" && method !== "HEAD") return new Response("method not allowed", { status: 405 })

    for (const mount of ["/sandbox", "/browser"])
      if (pathname === mount) return Response.redirect(`${mount}/${url.search}`, 301)

    if (pathname.startsWith("/browser/")) {
      if (!config.browserDist) return new Response("not found", { status: 404 })
      return serveFrom(config.browserDist, pathname.slice("/browser/".length), request)
    }

    if (pathname === "/sandbox/" || pathname === "/sandbox/index.html") {
      // The visitor's session starts on the page that uses it.
      const { setCookie } = session(request)
      const headers = new Headers()
      if (setCookie) headers.set("set-cookie", setCookie)
      return serveFrom(config.webDist, pathname.slice(1), request, headers)
    }

    return serveFrom(config.webDist, pathname.slice(1), request)
  }

  const server = Bun.serve<SocketData>({
    hostname: config.host,
    port: config.port,
    async fetch(request, server) {
      try {
        const response = await handle(request, server)
        if (response === undefined) return undefined as unknown as Response
        return withSecurityHeaders(response)
      } catch (error) {
        log(`request failed: ${(error as Error).stack ?? error}`)
        return withSecurityHeaders(json({ error: "internal error" }, { status: 500 }))
      }
    },
    websocket: {
      maxPayloadLength: 64 * 1024,
      backpressureLimit: 4 * 1024 * 1024,
      closeOnBackpressureLimit: true,
      idleTimeout: 120,
      sendPings: true,
      open(ws: ServerWebSocket<SocketData>) {
        ws.data.connection = hub.open(
          {
            sendText: (text) => void ws.send(text),
            sendBinary: (bytes) => void ws.send(bytes),
            close: (code, reason) => ws.close(code, reason),
          },
          ws.data.sid,
          ws.data.ip,
        )
      },
      message(ws, message) {
        const connection = ws.data.connection
        if (!connection) return
        if (typeof message === "string") {
          if (message.length > LIMITS.textFrameBytes) return connection.end(4000, "control frame too large")
          connection.onText(message)
        } else connection.onBinary(new Uint8Array(message))
      },
      close(ws) {
        ws.data.connection?.onClose()
      },
    },
  })

  return {
    server,
    hub,
    url: new URL(`http://${config.host.includes(":") ? `[${config.host}]` : config.host}:${server.port}/`),
    async stop() {
      hub.close()
      // Bun 1.3's stop() promise never settles once a WebSocket has been
      // served (the server does stop): don't wait on it for long.
      await Promise.race([server.stop(true), Bun.sleep(500)])
      await provider.close?.()
    },
  }
}
