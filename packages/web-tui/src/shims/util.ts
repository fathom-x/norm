// `util` / `node:util` for the browser build: the helpers opencode and its
// dependencies call, implemented on web primitives.
const custom = Symbol.for("nodejs.util.inspect.custom")

export function inspect(value: unknown, _options?: unknown): string {
  if (typeof value === "string") return `'${value}'`
  if (value instanceof Error) return value.stack ?? `${value.name}: ${value.message}`
  if (typeof value === "function") return `[Function: ${value.name || "anonymous"}]`
  if (typeof value === "bigint") return `${value}n`
  if (typeof value === "symbol") return value.toString()
  if (value && typeof value === "object" && custom in value) {
    const fn = (value as Record<symbol, unknown>)[custom]
    if (typeof fn === "function") return String(fn.call(value))
  }
  try {
    const seen = new WeakSet<object>()
    return (
      JSON.stringify(value, (_key, item) => {
        if (typeof item === "bigint") return `${item}n`
        if (item && typeof item === "object") {
          if (seen.has(item)) return "[Circular]"
          seen.add(item)
        }
        return item
      }) ?? String(value)
    )
  } catch {
    return String(value)
  }
}
inspect.custom = custom
inspect.defaultOptions = {}

export function format(template?: unknown, ...args: unknown[]): string {
  if (typeof template !== "string") return [template, ...args].map((item) => inspect(item)).join(" ")
  let index = 0
  const out = template.replace(/%[sdifjoOc%]/g, (token) => {
    if (token === "%%") return "%"
    if (index >= args.length) return token
    const arg = args[index++]
    if (token === "%s") return typeof arg === "string" ? arg : inspect(arg)
    if (token === "%d" || token === "%i") return String(token === "%i" ? Math.trunc(Number(arg)) : Number(arg))
    if (token === "%f") return String(Number(arg))
    if (token === "%c") return ""
    return inspect(arg)
  })
  return [out, ...args.slice(index).map((item) => (typeof item === "string" ? item : inspect(item)))].join(" ")
}

export const formatWithOptions = (_options: unknown, ...args: unknown[]) => format(...args)

type Callback = (error: unknown, value?: unknown) => void
const promisifyCustom = Symbol.for("nodejs.util.promisify.custom")

export function promisify(fn: (...args: any[]) => unknown) {
  const preset = (fn as unknown as Record<symbol, unknown>)[promisifyCustom]
  if (typeof preset === "function") return preset
  return (...args: unknown[]) =>
    new Promise((resolve, reject) => {
      fn(...args, ((error, value) => (error ? reject(error) : resolve(value))) satisfies Callback)
    })
}
promisify.custom = promisifyCustom

export function callbackify(fn: (...args: any[]) => Promise<unknown>) {
  return (...args: unknown[]) => {
    const callback = args.pop() as Callback
    fn(...args).then(
      (value) => callback(null, value),
      (error) => callback(error),
    )
  }
}

export function inherits(child: { prototype: object; super_?: unknown }, parent: { prototype: object }) {
  child.super_ = parent
  Object.setPrototypeOf(child.prototype, parent.prototype)
}

export const deprecate = <F>(fn: F) => fn
export const debuglog = () => Object.assign(() => undefined, { enabled: false })
export const debug = debuglog

export function isDeepStrictEqual(a: unknown, b: unknown): boolean {
  if (Object.is(a, b)) return true
  if (typeof a !== "object" || typeof b !== "object" || a === null || b === null) return false
  if (Object.getPrototypeOf(a) !== Object.getPrototypeOf(b)) return false
  if (a instanceof Date) return a.getTime() === (b as Date).getTime()
  if (a instanceof RegExp) return String(a) === String(b)
  if (a instanceof Map) {
    const other = b as Map<unknown, unknown>
    return a.size === other.size && [...a].every(([key, value]) => other.has(key) && isDeepStrictEqual(value, other.get(key)))
  }
  if (a instanceof Set) {
    const other = b as Set<unknown>
    return a.size === other.size && [...a].every((value) => other.has(value))
  }
  if (ArrayBuffer.isView(a)) {
    const left = new Uint8Array(a.buffer, a.byteOffset, a.byteLength)
    const right = new Uint8Array((b as ArrayBufferView).buffer, (b as ArrayBufferView).byteOffset, (b as ArrayBufferView).byteLength)
    return left.length === right.length && left.every((value, index) => value === right[index])
  }
  const left = Reflect.ownKeys(a)
  const right = Reflect.ownKeys(b)
  if (left.length !== right.length) return false
  return left.every(
    (key) =>
      Object.prototype.propertyIsEnumerable.call(b, key) ===
        Object.prototype.propertyIsEnumerable.call(a, key) &&
      isDeepStrictEqual((a as Record<PropertyKey, unknown>)[key], (b as Record<PropertyKey, unknown>)[key]),
  )
}

export const stripVTControlCharacters = (text: string) =>
  // eslint-disable-next-line no-control-regex
  text.replace(/\u001b\[[0-9;?]*[ -/]*[@-~]|\u001b\][^\u0007]*(\u0007|\u001b\\)/g, "")
export const styleText = (_format: unknown, text: string) => text
export const toUSVString = (text: string) => text.toWellFormed?.() ?? text
export const isArray = Array.isArray

const tag = (value: unknown) => Object.prototype.toString.call(value)
export const types = {
  isPromise: (value: unknown) => value instanceof Promise,
  isDate: (value: unknown) => value instanceof Date,
  isRegExp: (value: unknown) => value instanceof RegExp,
  isMap: (value: unknown) => value instanceof Map,
  isSet: (value: unknown) => value instanceof Set,
  isNativeError: (value: unknown) => value instanceof Error,
  isUint8Array: (value: unknown) => value instanceof Uint8Array,
  isTypedArray: (value: unknown) => ArrayBuffer.isView(value) && !(value instanceof DataView),
  isArrayBuffer: (value: unknown) => value instanceof ArrayBuffer,
  isAnyArrayBuffer: (value: unknown) => value instanceof ArrayBuffer || tag(value) === "[object SharedArrayBuffer]",
  isArrayBufferView: (value: unknown) => ArrayBuffer.isView(value),
  isAsyncFunction: (value: unknown) => tag(value) === "[object AsyncFunction]",
  isGeneratorFunction: (value: unknown) => /GeneratorFunction/.test(tag(value)),
  isProxy: () => false,
  isBoxedPrimitive: () => false,
}

export const TextEncoder = globalThis.TextEncoder
export const TextDecoder = globalThis.TextDecoder

export default {
  inspect,
  format,
  formatWithOptions,
  promisify,
  callbackify,
  inherits,
  deprecate,
  debuglog,
  debug,
  isDeepStrictEqual,
  stripVTControlCharacters,
  styleText,
  toUSVString,
  isArray,
  types,
  TextEncoder,
  TextDecoder,
}
