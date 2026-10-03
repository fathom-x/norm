import { describe, expect, test } from "bun:test"
import { installNodeTimers } from "../../src/shims/timers"

// A browser-like scope: timers are numbered, and clearing coerces the handle
// to a number the way WebIDL's `long` conversion does.
function browserScope() {
  const pending = new Map<number, ReturnType<typeof setTimeout>>()
  let next = 0
  const scope: Record<string, unknown> = {
    setTimeout: (fn: () => void, ms?: number) => {
      const id = ++next
      pending.set(id, setTimeout(fn, ms))
      return id
    },
    setInterval: (fn: () => void, ms?: number) => {
      const id = ++next
      pending.set(id, setInterval(fn, ms))
      return id
    },
  }
  const clear = (handle: unknown) => clearTimeout(pending.get(Number(handle)))
  return { scope, clear }
}

describe("installNodeTimers", () => {
  test("handles have Node's methods and clear as their numeric id", async () => {
    const { scope, clear } = browserScope()
    installNodeTimers(scope)
    const set = scope.setTimeout as (fn: () => void, ms?: number) => any
    const fired: string[] = []
    const kept = set(() => fired.push("kept"), 5)
    const cleared = set(() => fired.push("cleared"), 5)
    expect(kept.unref()).toBe(kept)
    expect(kept.hasRef()).toBe(true)
    expect(typeof Number(kept)).toBe("number")
    clear(cleared)
    await new Promise((resolve) => setTimeout(resolve, 30))
    expect(fired).toEqual(["kept"])
  })

  test("refresh restarts the timer", async () => {
    const { scope } = browserScope()
    installNodeTimers(scope)
    const set = scope.setTimeout as (fn: () => void, ms?: number) => any
    const fired: number[] = []
    const started = Date.now()
    const timer = set(() => fired.push(Date.now() - started), 20)
    await new Promise((resolve) => setTimeout(resolve, 10))
    timer.refresh()
    await new Promise((resolve) => setTimeout(resolve, 40))
    expect(fired.length).toBeGreaterThanOrEqual(1)
    expect(fired[fired.length - 1]).toBeGreaterThanOrEqual(25)
  })
})
