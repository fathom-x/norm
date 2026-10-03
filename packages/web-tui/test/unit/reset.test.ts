import { describe, expect, test } from "bun:test"
import { OPFS_STORES, resetBrowserState } from "../../src/reset"

function notFound() {
  const error = new Error("missing")
  error.name = "NotFoundError"
  return error
}

describe("start over", () => {
  test("stops the core first, deletes every store, then reloads", async () => {
    const log: string[] = []
    const present = new Set<string>(OPFS_STORES)
    await resetBrowserState({
      stopCore: () => log.push("stop"),
      root: async () => ({
        async removeEntry(name, options) {
          expect(options?.recursive).toBe(true)
          if (!present.delete(name)) throw notFound()
          log.push(`rm ${name}`)
        },
      }),
      reload: () => log.push("reload"),
    })
    expect(log).toEqual(["stop", ...OPFS_STORES.map((name) => `rm ${name}`), "reload"])
  })

  test("a store that was never created is fine, and a briefly locked one is retried", async () => {
    let busy = 2
    const removed: string[] = []
    await resetBrowserState({
      stopCore: () => {},
      root: async () => ({
        async removeEntry(name) {
          if (name === ".owallet-web") throw notFound()
          if (name === ".norm-sqlite" && busy-- > 0)
            throw Object.assign(new Error("locked"), { name: "NoModificationAllowedError" })
          removed.push(name)
        },
      }),
      reload: () => {},
    })
    expect(removed).toEqual(["norm-vfs", ".norm-sqlite"])
  })
})
