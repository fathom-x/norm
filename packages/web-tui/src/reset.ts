// "Start over" for the browser build: delete everything this page keeps in the
// origin private file system, then reload. Used by the setup screen when the
// wallet password is forgotten (it cannot be recovered).
//
// The core worker holds these stores open (OPFS sync access handles are
// exclusive), so it is stopped first.

/**
 * The OPFS directories the browser build owns, each named where it is opened:
 * the ZenFS file tree (src/vfs.ts, OPFS_DIRECTORY), the opencode database's
 * sahpool (packages/core/src/database/sqlite.browser.ts, `.${POOL_NAME}`) and
 * owallet-web's database pool (src/owallet.ts, OWALLET_OPFS_DIRECTORY).
 */
export const OPFS_STORES = ["norm-vfs", ".norm-sqlite", ".owallet-web"] as const

type Directory = { removeEntry(name: string, options?: { recursive?: boolean }): Promise<void> }

export async function resetBrowserState(input: {
  stopCore: () => void
  root?: () => Promise<Directory>
  reload?: () => void
}): Promise<void> {
  input.stopCore()
  const root = await (input.root ?? (() => navigator.storage.getDirectory()))()
  for (const name of OPFS_STORES) {
    // Handles can take a moment to release after the worker stops.
    for (let attempt = 0; ; attempt++) {
      try {
        await root.removeEntry(name, { recursive: true })
        break
      } catch (error) {
        if (isNotFound(error)) break
        if (attempt >= 10) throw error
        await new Promise((resolve) => setTimeout(resolve, 100 * (attempt + 1)))
      }
    }
  }
  ;(input.reload ?? (() => location.reload()))()
}

function isNotFound(error: unknown) {
  return error instanceof Error && error.name === "NotFoundError"
}
