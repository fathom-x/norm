// Browser twin of app-node-platform.ts, swapped in by the web build
// (packages/web-tui/vite.config.ts). Same exports, same Effect services:
// FileSystem is the ZenFS virtual file system (persisted to OPFS by the
// page), Path is Effect's portable POSIX implementation, HTTP is `fetch`.
import { fs } from "@zenfs/core"
import { LLMClient, RequestExecutor } from "@opencode-ai/llm/route"
import { FileSystem, Path } from "effect"
import { FetchHttpClient } from "effect/unstable/http"
import { HttpClient } from "effect/unstable/http"
import { makeGlobalNode } from "./app-node"
import { VfsFileSystem } from "./vfs-filesystem"

export const filesystem = makeGlobalNode({ service: FileSystem.FileSystem, layer: VfsFileSystem.layer(fs), deps: [] })
export const path = makeGlobalNode({ service: Path.Path, layer: Path.layer, deps: [] })
export const httpClient = makeGlobalNode({ service: HttpClient.HttpClient, layer: FetchHttpClient.layer, deps: [] })
export const requestExecutor = makeGlobalNode({
  service: RequestExecutor.Service,
  layer: RequestExecutor.layer,
  deps: [httpClient],
})
export const llmClient = makeGlobalNode({ service: LLMClient.Service, layer: LLMClient.layer, deps: [requestExecutor] })

export * as LayerNodePlatform from "./app-browser-platform"
