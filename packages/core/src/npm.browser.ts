// Browser twin of npm.ts, swapped in by the web build (packages/web-tui).
// There is no package manager in a tab (@npmcli/arborist needs a real disk,
// node:child_process and the npm registry over raw sockets), so:
// - `install` (the config dirs' background `@opencode-ai/plugin` install) has
//   nothing to do — that package is bundled — and succeeds;
// - `add` (runtime plugins, npm-hosted providers, LSP servers, formatters)
//   fails with InstallFailedError, which every caller already reports as
//   "could not install";
// - `which` finds nothing.
import { Context, Effect, Layer, Schema } from "effect"
import type { EffectFlock } from "./util/effect-flock"
import { makeGlobalNode } from "./effect/app-node"
import { filesystem } from "./effect/app-node-platform"

export const UNAVAILABLE = "Installing npm packages is not available in the browser build"

export class InstallFailedError extends Schema.TaggedErrorClass<InstallFailedError>()("NpmInstallFailedError", {
  add: Schema.Array(Schema.String).pipe(Schema.optional),
  dir: Schema.String,
  cause: Schema.optional(Schema.Defect()),
}) {}

export interface EntryPoint {
  readonly directory: string
  readonly entrypoint?: string
}

export interface Interface {
  readonly add: (pkg: string) => Effect.Effect<EntryPoint, InstallFailedError | EffectFlock.LockError>
  readonly install: (
    dir: string,
    input?: {
      add: {
        name: string
        version?: string
      }[]
    },
  ) => Effect.Effect<void, EffectFlock.LockError | InstallFailedError>
  readonly which: (pkg: string, bin?: string) => Effect.Effect<string | undefined>
}

export class Service extends Context.Service<Service, Interface>()("@opencode/Npm") {}

export const sanitize = (pkg: string) => pkg

const layer = Layer.succeed(
  Service,
  Service.of({
    add: (pkg) => Effect.fail(new InstallFailedError({ add: [pkg], dir: "", cause: new Error(UNAVAILABLE) })),
    install: () => Effect.void,
    which: () => Effect.succeed(undefined),
  }),
)

export const node = makeGlobalNode({ service: Service, layer, deps: [filesystem] })

export async function install(..._args: Parameters<Interface["install"]>) {}

export async function add(pkg: string): Promise<EntryPoint> {
  throw new InstallFailedError({ add: [pkg], dir: "", cause: new Error(UNAVAILABLE) })
}

export async function which(..._args: Parameters<Interface["which"]>) {
  return undefined
}

export * as Npm from "./npm.browser"
