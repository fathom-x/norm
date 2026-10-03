// Browser twin of worker.ts: the opencode server inside a dedicated Web
// Worker, answering the TUI's RPC protocol unchanged (`Rpc.listen` /
// `Rpc.emit`, `fetch` + `global.event`). Requests go straight to the pure
// HttpApi web handler; server.ts (node:http, NodeHttpServer, mDNS) is never
// imported. `server`, `snapshot` and `checkUpgrade` are left out: they need a
// listening socket, node:v8 and a package manager.
//
// Loaded by packages/web-tui/src/core.worker.ts, which first installs the
// fetch router (owallet.internal) and mounts the virtual file system, then
// imports this module — so nothing in the core can capture `fetch` or touch
// the file system before they exist.
import { Effect } from "effect"
import { GlobalBus } from "@/bus/global"
import { Config } from "@/config/config"
import { AppRuntime } from "@/effect/app-runtime"
import { InstanceRuntime } from "@/project/instance-runtime"
import { ServerAuth } from "@/server/auth"
import { disposeAllInstancesAndEmitGlobalDisposed } from "@/server/global-lifecycle"
import { HttpApiApp } from "@/server/routes/instance/httpapi/server"
import { Rpc } from "@/util/rpc"

// worker.ts swallows these through process.on(); a worker has no process, and
// an unhandled rejection is worth seeing in the devtools console.
self.addEventListener("unhandledrejection", (event) => {
  event.preventDefault()
  console.warn("[norm worker] unhandled rejection", event.reason)
})

GlobalBus.on("event", (event) => {
  Rpc.emit("global.event", event)
})

const handler = HttpApiApp.webHandler().handler

export const rpc = {
  async fetch(input: { url: string; method: string; headers: Record<string, string>; body?: string }) {
    const headers = { ...input.headers }
    const auth = ServerAuth.header()
    if (auth && !headers["authorization"] && !headers["Authorization"]) {
      headers["Authorization"] = auth
    }
    const request = new Request(input.url, {
      method: input.method,
      headers,
      body: input.body,
    })
    const response = await handler(request, HttpApiApp.context)
    const body = await response.text()
    return {
      status: response.status,
      headers: Object.fromEntries(response.headers.entries()),
      body,
    }
  },
  async reload() {
    await AppRuntime.runPromise(
      Effect.gen(function* () {
        const cfg = yield* Config.Service
        yield* cfg.invalidate()
        yield* disposeAllInstancesAndEmitGlobalDisposed({ swallowErrors: true })
      }),
    )
  },
  async shutdown() {
    await InstanceRuntime.disposeAllInstances()
  },
}

Rpc.listen(rpc)
// The page queues its calls until this arrives: messages posted before
// `Rpc.listen` installed `onmessage` would otherwise be dropped.
Rpc.emit("ready", {})
