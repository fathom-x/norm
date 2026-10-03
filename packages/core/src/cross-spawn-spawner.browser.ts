// Browser twin of cross-spawn-spawner.ts, swapped in by the web build. A tab
// has no processes: every spawn fails with `NotFound`, the same error a
// missing binary gives natively, so the callers that already tolerate an
// absent git / LSP server / formatter / ripgrep degrade the same way.
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"
import * as PlatformError from "effect/PlatformError"
import { ChildProcessSpawner, make as makeSpawner } from "effect/unstable/process/ChildProcessSpawner"
import { makeGlobalNode } from "./effect/app-node"
import { filesystem, path } from "./effect/app-node-platform"

export const UNAVAILABLE = "Running programs is not available in the browser build"

export const make = Effect.succeed(
  makeSpawner((command) =>
    Effect.fail(
      PlatformError.systemError({
        _tag: "NotFound",
        module: "ChildProcess",
        method: "spawn",
        pathOrDescriptor: command._tag === "StandardCommand" ? command.command : "pipeline",
        description: UNAVAILABLE,
      }),
    ),
  ),
)

const layer = Layer.effect(ChildProcessSpawner, make)

export const node = makeGlobalNode({ service: ChildProcessSpawner, layer, deps: [filesystem, path] })

export * as CrossSpawnSpawner from "./cross-spawn-spawner.browser"
