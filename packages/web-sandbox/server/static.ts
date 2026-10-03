// Static files: the landing page and /sandbox/ (this package's Vite build),
// and optionally variant A's build under /browser/. Paths are resolved inside
// their root only; directories serve their index.html; Vite's hashed assets
// are cached for a year, HTML never.
import { existsSync, statSync } from "node:fs"
import path from "node:path"

const TYPES: Record<string, string> = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".json": "application/json",
  ".map": "application/json",
  ".webmanifest": "application/manifest+json",
  ".wasm": "application/wasm",
  ".svg": "image/svg+xml",
  ".png": "image/png",
  ".jpg": "image/jpeg",
  ".ico": "image/x-icon",
  ".woff": "font/woff",
  ".woff2": "font/woff2",
  ".ttf": "font/ttf",
  ".txt": "text/plain; charset=utf-8",
}

export function contentType(file: string): string {
  return TYPES[path.extname(file).toLowerCase()] ?? "application/octet-stream"
}

/** The file `urlPath` (already stripped of its mount prefix) names inside `root`, if any. */
export function resolveFile(root: string, urlPath: string): string | undefined {
  let decoded: string
  try {
    decoded = decodeURIComponent(urlPath)
  } catch {
    return undefined
  }
  if (decoded.includes("\0")) return undefined
  const file = path.resolve(root, `.${path.posix.normalize(`/${decoded}`)}`)
  if (file !== root && !file.startsWith(root + path.sep)) return undefined
  if (!existsSync(file)) return undefined
  const stat = statSync(file)
  if (stat.isDirectory()) {
    const index = path.join(file, "index.html")
    return existsSync(index) ? index : undefined
  }
  return stat.isFile() ? file : undefined
}

/** Hashed build output (Vite's `assets/name-<hash>.ext`): safe to cache forever. */
function immutable(urlPath: string) {
  return /(^|\/)assets\/[^/]+-[A-Za-z0-9_-]{8,}\.[a-z0-9]+$/.test(urlPath)
}

export function fileResponse(file: string, urlPath: string, method: string, headers: Headers = new Headers()) {
  const type = contentType(file)
  headers.set("content-type", type)
  if (type.startsWith("text/html")) headers.set("cache-control", "no-cache")
  else if (immutable(urlPath)) headers.set("cache-control", "public, max-age=31536000, immutable")
  else headers.set("cache-control", "public, max-age=300")
  const body = Bun.file(file)
  headers.set("content-length", String(body.size))
  return new Response(method === "HEAD" ? null : body, { headers })
}
