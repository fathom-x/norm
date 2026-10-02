// node:url for the browser.
export const URL = globalThis.URL
export const URLSearchParams = globalThis.URLSearchParams
export function fileURLToPath(url: string | URL): string {
  const value = typeof url === "string" ? url : url.href
  if (value.startsWith("file://")) return decodeURIComponent(value.slice("file://".length))
  try {
    return decodeURIComponent(new globalThis.URL(value).pathname)
  } catch {
    return value
  }
}
export function pathToFileURL(path: string): URL {
  return new globalThis.URL(`file://${encodeURI(path)}`)
}
export default { URL, URLSearchParams, fileURLToPath, pathToFileURL }
