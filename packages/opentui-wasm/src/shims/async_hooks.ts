export class AsyncLocalStorage<T> {
  private store: T | undefined
  getStore(): T | undefined {
    return this.store
  }
  run<R>(store: T, fn: (...args: any[]) => R, ...args: any[]): R {
    const prev = this.store
    this.store = store
    try {
      return fn(...args)
    } finally {
      this.store = prev
    }
  }
  enterWith(store: T) {
    this.store = store
  }
  disable() {
    this.store = undefined
  }
}
export default { AsyncLocalStorage }
