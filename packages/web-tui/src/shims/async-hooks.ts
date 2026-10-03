// `async_hooks` for the browser build. There is no async context propagation
// in browsers (AsyncContext is not shipped yet), so AsyncLocalStorage is
// synchronous-scope only: `getStore()` sees the value inside `run()`'s
// synchronous extent, not across awaits. opencode's uses tolerate that:
// WorkspaceContext falls back to "no workspace" and Effect carries its own
// fiber context.
export class AsyncLocalStorage<T> {
  private store: T | undefined
  getStore() {
    return this.store
  }
  run<R>(store: T, fn: (...args: unknown[]) => R, ...args: unknown[]): R {
    const previous = this.store
    this.store = store
    try {
      return fn(...args)
    } finally {
      this.store = previous
    }
  }
  enterWith(store: T) {
    this.store = store
  }
  exit<R>(fn: (...args: unknown[]) => R, ...args: unknown[]): R {
    const previous = this.store
    this.store = undefined
    try {
      return fn(...args)
    } finally {
      this.store = previous
    }
  }
  disable() {
    this.store = undefined
  }
  static bind<F>(fn: F) {
    return fn
  }
  static snapshot() {
    return <R>(fn: (...args: unknown[]) => R, ...args: unknown[]) => fn(...args)
  }
}

export class AsyncResource {
  constructor(readonly type: string) {}
  runInAsyncScope<R>(fn: (...args: unknown[]) => R, thisArg?: unknown, ...args: unknown[]): R {
    return fn.apply(thisArg, args)
  }
  bind<F>(fn: F) {
    return fn
  }
  emitDestroy() {
    return this
  }
  static bind<F>(fn: F) {
    return fn
  }
}

export const createHook = () => ({ enable: () => undefined, disable: () => undefined })
export const executionAsyncId = () => 0
export const triggerAsyncId = () => 0

export default { AsyncLocalStorage, AsyncResource, createHook, executionAsyncId, triggerAsyncId }
