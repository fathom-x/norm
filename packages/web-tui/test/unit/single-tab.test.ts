import { describe, expect, test } from "bun:test"
import { claimTab, TAB_LOCK } from "../../src/single-tab"

/** A one-holder lock manager, enough for claimTab's two request shapes. */
function fakeLocks() {
  let held = false
  const queue: (() => void)[] = []
  const locks = {
    request(name: string, ...args: any[]) {
      expect(name).toBe(TAB_LOCK)
      const [options, callback] = args.length === 2 ? args : [{}, args[0]]
      const run = () => {
        held = true
        return Promise.resolve(callback({ name })).finally(() => {
          held = false
          queue.shift()?.()
        })
      }
      if (!held) return run()
      if (options.ifAvailable) return Promise.resolve(callback(null))
      return new Promise((resolve) => queue.push(() => resolve(run())))
    },
  }
  return { locks: locks as any, release: () => queue.length && ((held = false), queue.shift()!()) }
}

describe("one tab at a time", () => {
  test("the first tab gets the stores straight away", async () => {
    const { locks } = fakeLocks()
    let waited = false
    await claimTab(() => (waited = true), locks)
    expect(waited).toBe(false)
  })

  test("a second tab says it is waiting, and takes over when the first goes away", async () => {
    const { locks, release } = fakeLocks()
    await claimTab(() => {}, locks)
    let waited = false
    let owned = false
    void claimTab(() => (waited = true), locks).then(() => (owned = true))
    await new Promise((resolve) => setTimeout(resolve, 0))
    expect(waited).toBe(true)
    expect(owned).toBe(false)
    release() // the first tab closes
    await new Promise((resolve) => setTimeout(resolve, 0))
    expect(owned).toBe(true)
  })

  test("without Web Locks there is nothing to wait for", async () => {
    await claimTab(() => {
      throw new Error("should not wait")
    }, undefined)
  })
})
