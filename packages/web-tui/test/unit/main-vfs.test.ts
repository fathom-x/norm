import { expect, test } from "bun:test"
import { configure, fs, InMemory, mounts } from "@zenfs/core"
import { mountWorkerVfs, serveVfs } from "../../src/main-vfs"

// Both ends in one thread: the "worker" tree is an in-memory root served on a
// BroadcastChannel, then the "page" remounts / over that channel (Port).
test("the page mounts the worker's tree over a channel", async () => {
  await configure({ mounts: { "/": InMemory }, disableAccessChecks: true })
  await fs.promises.mkdir("/norm/data", { recursive: true })
  await fs.promises.writeFile("/norm/data/auth.json", '{"overpay":{"type":"api","key":"owk_1"}}')
  const worker = mounts.get("/")!
  const name = `norm-vfs-test-${crypto.randomUUID()}`
  serveVfs(name)

  await mountWorkerVfs(name, 5_000)
  expect(mounts.get("/")).not.toBe(worker)

  // Async reads go to the worker; sync reads hit the preloaded cache.
  expect(JSON.parse(await fs.promises.readFile("/norm/data/auth.json", "utf8")).overpay.key).toBe("owk_1")
  expect(fs.existsSync("/norm/data/auth.json")).toBe(true)

  // Page writes land in the worker's tree.
  await fs.promises.mkdir("/norm/state", { recursive: true })
  await fs.promises.writeFile("/norm/state/kv.json", '{"theme":"norm"}')
  const bytes = new Uint8Array(16)
  await worker.read("/norm/state/kv.json", bytes, 0, 16)
  expect(new TextDecoder().decode(bytes)).toBe('{"theme":"norm"}')

  // /tmp stays private to the page.
  await fs.promises.writeFile("/tmp/scratch", "x")
  expect(await worker.exists("/tmp/scratch")).toBe(false)
})
