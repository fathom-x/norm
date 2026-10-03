// `timers/promises` for the browser build.
export function setTimeout<T>(delay = 0, value?: T, options?: { signal?: AbortSignal }) {
  return new Promise<T | undefined>((resolve, reject) => {
    if (options?.signal?.aborted) return reject(options.signal.reason)
    const timer = globalThis.setTimeout(() => resolve(value), delay)
    options?.signal?.addEventListener("abort", () => {
      globalThis.clearTimeout(timer)
      reject(options.signal?.reason)
    })
  })
}

export const setImmediate = <T>(value?: T) => setTimeout(0, value)

export async function* setInterval<T>(delay = 0, value?: T) {
  while (true) yield await setTimeout(delay, value)
}

export const scheduler = { wait: (delay: number) => setTimeout(delay), yield: () => setTimeout(0) }

export default { setTimeout, setImmediate, setInterval, scheduler }
