// Which Overpay a `?overpay=<url>` link may point the page at. The wallet in
// this tab persists, holds keys and pays for things, so a link must not be
// able to re-point it at an arbitrary "marketplace" (a look-alike login, its
// own Lightning invoices). Allowed: the known Overpay deployments, anything
// listed at build time (NORM_WEB_OVERPAY_URLS, comma-separated), and — for
// development and the test suites — a loopback Overpay when the page itself
// is served from loopback.

/** The known Overpay deployments: production and the pre-release staging. */
export const KNOWN_OVERPAY = ["https://overpay.com", "https://overpay-eykm.onrender.com"]

declare const __NORM_WEB_OVERPAY_URLS__: string | undefined

function buildTimeExtras(): string[] {
  const raw = typeof __NORM_WEB_OVERPAY_URLS__ === "string" ? __NORM_WEB_OVERPAY_URLS__ : ""
  return raw
    .split(",")
    .map((entry) => entry.trim())
    .filter(Boolean)
}

const LOOPBACK = new Set(["localhost", "127.0.0.1", "[::1]"])

/**
 * The Overpay origin `requested` names when the page may use it, else
 * undefined (the page keeps its default and says why in the console).
 */
export function allowedOverpay(
  requested: string | null | undefined,
  page: { hostname: string } = location,
  allowed: string[] = [...KNOWN_OVERPAY, ...buildTimeExtras()],
): string | undefined {
  if (!requested) return undefined
  let url: URL
  try {
    url = new URL(requested)
  } catch {
    return refuse(requested, "not a URL")
  }
  if (url.username || url.password) return refuse(requested, "credentials in the URL")
  if (LOOPBACK.has(page.hostname) && LOOPBACK.has(url.hostname)) return url.origin
  if (url.protocol !== "https:") return refuse(requested, "not https")
  const origins = new Set(
    allowed.flatMap((entry) => {
      try {
        return [new URL(entry).origin]
      } catch {
        return []
      }
    }),
  )
  return origins.has(url.origin) ? url.origin : refuse(requested, "not a known Overpay")
}

function refuse(requested: string, why: string): undefined {
  console.warn(`[norm] ignoring ?overpay=${requested} (${why}); using the default Overpay`)
  return undefined
}
