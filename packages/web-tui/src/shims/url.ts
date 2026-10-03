// `url` / `node:url` for the browser build: WHATWG URL plus Node's file-URL
// helpers (POSIX only).
export const URL = globalThis.URL
export const URLSearchParams = globalThis.URLSearchParams

// Node rejects non-file URLs. Bundled modules compute their own location as
// `fileURLToPath(new URL(".", import.meta.url))` at import time, and in a
// browser import.meta.url is http(s): answer with the path part instead of
// failing the whole import.
export function fileURLToPath(input: string | URL) {
  const url = typeof input === "string" ? new globalThis.URL(input) : input
  return decodeURIComponent(url.pathname)
}

export function pathToFileURL(path: string) {
  const absolute = path.startsWith("/") ? path : `${globalThis.process?.cwd?.() ?? "/"}/${path}`
  return new globalThis.URL(`file://${absolute.split("/").map(encodeURIComponent).join("/")}`)
}

export function format(input: URL | { href?: string } | string) {
  if (typeof input === "string") return input
  return input.href ?? String(input)
}

export function parse(input: string) {
  const url = new globalThis.URL(input, "http://invalid.invalid")
  const relative = url.host === "invalid.invalid"
  return {
    href: relative ? input : url.href,
    protocol: relative ? null : url.protocol,
    host: relative ? null : url.host,
    hostname: relative ? null : url.hostname,
    port: relative ? null : url.port || null,
    pathname: url.pathname,
    search: url.search || null,
    query: url.search ? url.search.slice(1) : null,
    hash: url.hash || null,
    path: url.pathname + url.search,
    auth: url.username ? `${url.username}${url.password ? `:${url.password}` : ""}` : null,
  }
}

export function resolve(from: string, to: string) {
  return new globalThis.URL(to, new globalThis.URL(from, "resolve://")).href.replace(/^resolve:\/\//, "")
}

export const domainToASCII = (domain: string) => new globalThis.URL(`http://${domain}`).hostname
export const domainToUnicode = (domain: string) => domain

export default {
  URL,
  URLSearchParams,
  fileURLToPath,
  pathToFileURL,
  format,
  parse,
  resolve,
  domainToASCII,
  domainToUnicode,
}
