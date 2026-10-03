// `@effect/platform-node` (the barrel) for the browser build. Only the
// FileSystem and Path layers have browser meaning; the rest (HTTP server,
// sockets, child processes, Node streams) are reached only from code the
// browser build replaces or never runs, and fail loudly if called.
const unavailable = (name: string) =>
  new Proxy(
    {},
    {
      get: (_, key) => {
        if (key === "then") return undefined
        throw new Error(`@effect/platform-node ${name}.${String(key)} is not available in the browser build`)
      },
    },
  )

export * as NodeFileSystem from "./effect-node-filesystem"
export * as NodePath from "./effect-node-path"
export const NodeSink = unavailable("NodeSink")
export const NodeStream = unavailable("NodeStream")
export const NodeHttpServer = unavailable("NodeHttpServer")
export const NodeRuntime = unavailable("NodeRuntime")
export const NodeServices = unavailable("NodeServices")
export const NodeChildProcessSpawner = unavailable("NodeChildProcessSpawner")
