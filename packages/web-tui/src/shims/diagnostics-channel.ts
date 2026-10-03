// `diagnostics_channel` for the browser build: channels exist and accept
// subscribers, but nothing in the browser build publishes diagnostics.
function makeChannel(name: string) {
  const subscribers = new Set<(message: unknown, name: string) => void>()
  return {
    name,
    get hasSubscribers() {
      return subscribers.size > 0
    },
    publish: (message: unknown) => subscribers.forEach((fn) => fn(message, name)),
    subscribe: (fn: (message: unknown, name: string) => void) => void subscribers.add(fn),
    unsubscribe: (fn: (message: unknown, name: string) => void) => subscribers.delete(fn),
    bindStore: () => undefined,
    unbindStore: () => false,
    runStores: <R>(_data: unknown, fn: (...args: unknown[]) => R, thisArg?: unknown, ...args: unknown[]) =>
      fn.apply(thisArg, args),
  }
}

const channels = new Map<string, ReturnType<typeof makeChannel>>()

export function channel(name: string) {
  const existing = channels.get(name)
  if (existing) return existing
  const created = makeChannel(name)
  channels.set(name, created)
  return created
}

export const hasSubscribers = (name: string) => channel(name).hasSubscribers
export const subscribe = (name: string, fn: (message: unknown, name: string) => void) => channel(name).subscribe(fn)
export const unsubscribe = (name: string, fn: (message: unknown, name: string) => void) => channel(name).unsubscribe(fn)

export function tracingChannel(name: string) {
  const call = <R>(fn: (...args: unknown[]) => R, _context?: unknown, thisArg?: unknown, ...args: unknown[]) =>
    fn.apply(thisArg, args)
  return {
    start: channel(`tracing:${name}:start`),
    end: channel(`tracing:${name}:end`),
    asyncStart: channel(`tracing:${name}:asyncStart`),
    asyncEnd: channel(`tracing:${name}:asyncEnd`),
    error: channel(`tracing:${name}:error`),
    hasSubscribers: false,
    subscribe: () => undefined,
    unsubscribe: () => undefined,
    traceSync: call,
    tracePromise: call,
    traceCallback: call,
  }
}

export default { channel, hasSubscribers, subscribe, unsubscribe, tracingChannel }
