// `fs` / `node:fs` for the browser build: ZenFS, the same tree the Effect
// FileSystem layer (core's vfs-filesystem.ts) serves. Mounts are configured by
// ../vfs.ts before the core is imported.
import { fs } from "@zenfs/core"

export * from "@zenfs/core/emulation/index.js"
export default fs
