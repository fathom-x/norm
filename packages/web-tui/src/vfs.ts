// The browser build's file system: ZenFS, mounted before the core is
// imported. In a dedicated worker with OPFS the tree persists in the origin
// private file system (directory `norm-vfs`, through @zenfs/dom's WebAccess
// backend); anywhere else it is in memory. /tmp is always in memory.
//
// SQLite does not live here: sqlite.browser.ts keeps its database in its own
// OPFS sahpool directory, which ZenFS never sees.
import { configure, defaultContext, fs, InMemory } from "@zenfs/core"
import { WebAccess } from "@zenfs/dom"
import { WORKSPACE } from "./env"
import { DEMO_WORKSPACE } from "./demo-workspace"

export type Storage = "opfs" | "memory"

const OPFS_DIRECTORY = "norm-vfs"

export async function mountVfs(options: { persist?: boolean } = {}): Promise<{ storage: Storage; seeded: boolean }> {
  const handle = options.persist === false ? undefined : await opfsDirectory()
  await configure({
    mounts: {
      "/": handle ? { backend: WebAccess, handle } : InMemory,
      "/tmp": InMemory,
    },
    disableAccessChecks: true,
  })
  defaultContext.pwd = WORKSPACE
  return { storage: handle ? "opfs" : "memory", seeded: await seedWorkspace() }
}

async function opfsDirectory() {
  const storage = globalThis.navigator?.storage
  if (!storage?.getDirectory) return undefined
  return storage
    .getDirectory()
    .then((root) => root.getDirectoryHandle(OPFS_DIRECTORY, { create: true }))
    .catch((error: unknown) => {
      console.warn("[vfs] OPFS unavailable, using memory", error)
      return undefined
    })
}

/**
 * Writes the demo project on first run (or when /workspace was deleted);
 * never overwrites what the user has changed.
 */
export async function seedWorkspace(files: Record<string, string> = DEMO_WORKSPACE) {
  if (fs.existsSync(WORKSPACE)) return false
  for (const [relative, content] of Object.entries(files)) {
    const path = `${WORKSPACE}/${relative}`
    await fs.promises.mkdir(path.slice(0, path.lastIndexOf("/")), { recursive: true })
    await fs.promises.writeFile(path, content)
  }
  return true
}
