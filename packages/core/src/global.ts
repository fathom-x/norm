import path from "path"
import fs from "fs/promises"
import { xdgData, xdgCache, xdgConfig, xdgState } from "xdg-basedir"
import os from "os"
import { Context, Effect, Layer } from "effect"
import { Flock } from "./util/flock"
import { Flag } from "./flag/flag"
import { makeGlobalNode } from "./effect/app-node"

// norm fork: own app identity so norm's XDG dirs (config, data incl.
// auth.json, cache, state) never collide with a stock opencode install
// on the same machine.
const app = "norm"

// `NORM_HOME=/tmp/example` collapses norm's whole per-user footprint into one
// directory: the four XDG dirs below, plus — in `src/norm/norm.ts` — the
// owallet wallet database, owallet's state and config dirs, the bundled
// binaries, and the port the auto-started `owallet serve` listens on. Nothing
// outside it is read or written, so a throwaway root is a genuinely fresh
// install that leaves the real ~/.owallet, ~/.norm and XDG dirs untouched.
// It relocates norm's own state, not the workspace: project files and project
// config are still read from where they are.
// Read once at import (these paths are module constants); export it in the
// shell before launching norm.
const sandbox = process.env["NORM_HOME"]?.trim()
const root = sandbox ? path.resolve(sandbox) : undefined
const data = root ? path.join(root, "data") : path.join(xdgData!, app)
const cache = root ? path.join(root, "cache") : path.join(xdgCache!, app)
const config = root ? path.join(root, "config") : path.join(xdgConfig!, app)
const state = root ? path.join(root, "state") : path.join(xdgState!, app)
const tmp = root ? path.join(root, "tmp") : path.join(os.tmpdir(), app)

const paths = {
  get home() {
    return process.env.OPENCODE_TEST_HOME ?? os.homedir()
  },
  data,
  bin: path.join(cache, "bin"),
  log: path.join(data, "log"),
  repos: path.join(data, "repos"),
  cache,
  config,
  state,
  tmp,
}

export const Path = paths

Flock.setGlobal({ state })

await Promise.all([
  fs.mkdir(Path.data, { recursive: true }),
  fs.mkdir(Path.config, { recursive: true }),
  fs.mkdir(Path.state, { recursive: true }),
  fs.mkdir(Path.tmp, { recursive: true }),
  fs.mkdir(Path.log, { recursive: true }),
  fs.mkdir(Path.bin, { recursive: true }),
  fs.mkdir(Path.repos, { recursive: true }),
])

export class Service extends Context.Service<Service, Interface>()("@opencode/Global") {}

export interface Interface {
  readonly home: string
  readonly data: string
  readonly cache: string
  readonly config: string
  readonly state: string
  readonly tmp: string
  readonly bin: string
  readonly log: string
  readonly repos: string
}

export function make(input: Partial<Interface> = {}): Interface {
  return {
    home: Path.home,
    data: Path.data,
    cache: Path.cache,
    config: Flag.OPENCODE_CONFIG_DIR ?? Path.config,
    state: Path.state,
    tmp: Path.tmp,
    bin: Path.bin,
    log: Path.log,
    repos: Path.repos,
    ...input,
  }
}

const layer = Layer.effect(
  Service,
  Effect.sync(() => Service.of(make())),
)

export const node = makeGlobalNode({ service: Service, layer: layer, deps: [] })

export const layerWith = (input: Partial<Interface>) =>
  Layer.effect(
    Service,
    Effect.sync(() => Service.of(make(input))),
  )

export * as Global from "./global"
