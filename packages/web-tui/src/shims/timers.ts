// Node-style timer handles for the page: the TUI calls `.unref()` /
// `.ref()` / `.refresh()` on what setTimeout returns (Node's Timeout), while
// browsers return a number. The handle converts to its numeric id, so the
// native clearTimeout/clearInterval accept it unchanged (WebIDL coerces with
// ToNumber).
type Timer = (handler: TimerHandler, timeout?: number, ...args: unknown[]) => number

class Timeout {
  #current: number
  readonly #restart: () => number
  constructor(id: number, restart: () => number) {
    this.#current = id
    this.#restart = restart
  }
  ref() {
    return this
  }
  unref() {
    return this
  }
  hasRef() {
    return true
  }
  refresh() {
    clearTimeout(this.#current)
    this.#current = this.#restart()
    return this
  }
  close() {
    clearTimeout(this.#current)
    return this
  }
  [Symbol.toPrimitive]() {
    return this.#current
  }
}

export function installNodeTimers(scope: Record<string, unknown> = globalThis as never) {
  if ((scope.setTimeout as { node?: true }).node) return
  const wrap = (native: Timer) =>
    Object.assign(
      (handler: TimerHandler, timeout?: number, ...args: unknown[]) => {
        const start = () => native.call(scope, handler, timeout, ...args)
        return new Timeout(start(), start) as unknown as number
      },
      { node: true as const },
    )
  scope.setTimeout = wrap(scope.setTimeout as Timer)
  scope.setInterval = wrap(scope.setInterval as Timer)
}
