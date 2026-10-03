// The page's file system: the core worker's ZenFS tree, mounted over a
// BroadcastChannel with ZenFS's Port backend (the worker side calls
// `serveVfs`). The TUI's own files — KV state, prompt history, themes,
// norm's auth.json read by the owallet sidebar — therefore land in the same
// OPFS-backed tree the core uses, and `fs` / `fs/promises` / `Bun.file` on the
// page all go through it.
//
// Port is an async backend with a synchronous in-memory cache: mounting
// preloads the tree, async calls go to the worker (always current), and sync
// calls (`existsSync`, `realpathSync`, ...) read the cache, so a sync read
// sees what the page wrote but not what the worker wrote after the mount. The
// TUI only uses sync calls for path checks, which that serves fine.
import { attachFS, configure, defaultContext, InMemory, mounts, Port } from "@zenfs/core"
import { WORKSPACE } from "./env"

/** Worker side: answer the page's file-system calls from the mounted root. */
export function serveVfs(name: string) {
  const root = mounts.get("/")
  if (!root) throw new Error("serveVfs: mount the VFS first")
  attachFS(new BroadcastChannel(name), root)
}

/** Page side: mount the worker's tree at `/` (and a private in-memory /tmp). */
export async function mountWorkerVfs(name: string, timeout = 30_000) {
  // Port.create, not a `{ backend: Port }` config: configure's option check
  // wants an EventTarget, which a BroadcastChannel is not under every runtime.
  const worker = Port.create({ port: new BroadcastChannel(name), timeout })
  await configure({
    mounts: {
      "/": worker,
      "/tmp": InMemory,
    },
    disableAccessChecks: true,
  })
  defaultContext.pwd = WORKSPACE
}
