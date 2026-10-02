// node:util for the browser (the subset @opentui/core uses).
export function inspect(value: unknown, _options?: unknown): string {
  if (typeof value === "string") return value
  try {
    const seen = new WeakSet()
    return JSON.stringify(
      value,
      (_key, v) => {
        if (typeof v === "bigint") return `${v}n`
        if (typeof v === "function") return `[Function ${v.name || "anonymous"}]`
        if (typeof v === "object" && v !== null) {
          if (seen.has(v)) return "[Circular]"
          seen.add(v)
        }
        return v
      },
      2,
    )
  } catch {
    return String(value)
  }
}
inspect.custom = Symbol.for("nodejs.util.inspect.custom")
export function format(fmt: unknown, ...args: unknown[]): string {
  if (typeof fmt !== "string") return [fmt, ...args].map((a) => inspect(a)).join(" ")
  let i = 0
  const out = fmt.replace(/%[sdifjoO%]/g, (m) => {
    if (m === "%%") return "%"
    if (i >= args.length) return m
    const a = args[i++]
    if (m === "%d" || m === "%i") return String(Number(a))
    if (m === "%f") return String(parseFloat(String(a)))
    if (m === "%s") return typeof a === "string" ? a : inspect(a)
    return inspect(a)
  })
  return [out, ...args.slice(i).map((a) => (typeof a === "string" ? a : inspect(a)))].join(" ")
}
export function isDeepStrictEqual(a: unknown, b: unknown): boolean {
  if (Object.is(a, b)) return true
  if (typeof a !== "object" || typeof b !== "object" || a === null || b === null) return false
  if (Array.isArray(a) !== Array.isArray(b)) return false
  const ka = Object.keys(a as object)
  const kb = Object.keys(b as object)
  if (ka.length !== kb.length) return false
  return ka.every((k) => isDeepStrictEqual((a as any)[k], (b as any)[k]))
}
export function promisify(fn: (...args: any[]) => void) {
  return (...args: any[]) =>
    new Promise((resolve, reject) => fn(...args, (err: unknown, value: unknown) => (err ? reject(err) : resolve(value))))
}
export function inherits(ctor: any, superCtor: any) {
  Object.setPrototypeOf(ctor.prototype, superCtor.prototype)
  Object.setPrototypeOf(ctor, superCtor)
}
export function deprecate<T>(fn: T): T {
  return fn
}
export function debuglog() {
  return () => {}
}
export function parseArgs() {
  return { values: {}, positionals: [] }
}
export const types = {
  isPromise: (v: unknown) => v instanceof Promise,
  isUint8Array: (v: unknown) => v instanceof Uint8Array,
}
export const TextEncoder = globalThis.TextEncoder
export const TextDecoder = globalThis.TextDecoder
export default {
  inspect,
  format,
  isDeepStrictEqual,
  promisify,
  inherits,
  deprecate,
  debuglog,
  parseArgs,
  types,
  TextEncoder,
  TextDecoder,
}
