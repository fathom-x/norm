// `@effect/platform-node/NodeFileSystem` for the browser build: the VFS layer.
import { fs } from "@zenfs/core"
import { VfsFileSystem } from "@opencode-ai/core/effect/vfs-filesystem"

export const layer = VfsFileSystem.layer(fs)
