// Vite `resolve.alias` entries that let @opentui/core (and @opentui/solid)
// run in a browser on the wasm core: Node built-ins map to small shims in
// ./shims and the packages themselves map to the patched opentui sources.
// ("#opentui/runtime-assets" resolves through its "browser" condition, added
// by the patches.)
import { createRequire } from "node:module"
import { fileURLToPath } from "node:url"
import path from "node:path"
import fs from "node:fs"

export interface OpentuiWasmAliasOptions {
  /**
   * Built packages from scripts/build.sh (`dist/`: core/index.browser.js,
   * core/wasm.js, solid/index.js). This is what an app should consume.
   */
  dist?: string
  /** Or: the patched opentui checkout itself (.work/opentui), for working on the patches. */
  opentuiRoot?: string
}

const here = path.dirname(fileURLToPath(import.meta.url))
const shim = (name: string) => path.join(here, "shims", name)

function packageAliases({ dist, opentuiRoot }: OpentuiWasmAliasOptions) {
  if (dist) {
    return [
      { find: /^@opentui\/core\/wasm$/, replacement: path.join(dist, "core/wasm.js") },
      { find: /^@opentui\/core\/testing$/, replacement: shim("testing-unavailable.ts") },
      { find: /^@opentui\/core$/, replacement: path.join(dist, "core/index.browser.js") },
      { find: /^@opentui\/solid$/, replacement: path.join(dist, "solid/index.js") },
      {
        find: /^@opentui\/solid\/(jsx-runtime|jsx-dev-runtime|components)$/,
        replacement: path.join(dist, "solid/$1.js"),
      },
    ]
  }
  if (!opentuiRoot) throw new Error("opentuiWasmAliases needs `dist` or `opentuiRoot`")
  const core = path.join(opentuiRoot, "packages/core/src")
  const solid = path.join(opentuiRoot, "packages/solid")
  return [
    { find: /^@opentui\/core\/wasm$/, replacement: path.join(core, "platform/ffi-wasm.ts") },
    { find: /^@opentui\/core\/testing$/, replacement: shim("testing-unavailable.ts") },
    { find: /^@opentui\/core$/, replacement: path.join(core, "index.ts") },
    { find: /^@opentui\/core\/(.*)$/, replacement: path.join(core, "$1") },
    { find: /^@opentui\/solid$/, replacement: path.join(solid, "index.ts") },
    { find: /^@opentui\/solid\/(jsx-runtime|jsx-dev-runtime|components)$/, replacement: path.join(solid, "$1.ts") },
  ]
}

export function opentuiWasmAliases(options: OpentuiWasmAliasOptions) {
  const requireFromHere = createRequire(path.join(here, "../demo/package.json"))
  const packageDir = (name: string) => {
    // Some packages do not export ./package.json; walk up from the entry.
    let dir = path.dirname(requireFromHere.resolve(name))
    while (!(path.basename(dir) === name && fs.existsSync(path.join(dir, "package.json")))) {
      const parent = path.dirname(dir)
      if (parent === dir) throw new Error(`cannot locate the ${name} package directory`)
      dir = parent
    }
    return dir
  }
  const builtin = (name: string, target: string) => ({ find: new RegExp(`^(node:)?${name}$`), replacement: target })
  // The built @opentui/solid imports its runtime dependencies by bare
  // specifier; resolve them from the app, and to the same solid-js file the
  // app's own components use (one solid-js instance).
  const solidDir = packageDir("solid-js")
  const distDeps = options.dist
    ? [
        { find: /^solid-js\/dist\/solid\.js$/, replacement: path.join(solidDir, "dist/solid.js") },
        { find: /^solid-js\/store$/, replacement: path.join(solidDir, "store/dist/store.js") },
        { find: /^entities$/, replacement: path.join(packageDir("entities"), "dist/esm/index.js") },
      ]
    : []
  return [
    ...packageAliases(options),
    ...distDeps,
    builtin("fs\\/promises", shim("fs-promises.ts")),
    builtin("fs", shim("fs.ts")),
    builtin("os", shim("os.ts")),
    builtin("url", shim("url.ts")),
    builtin("module", shim("module.ts")),
    builtin("util", shim("util.ts")),
    builtin("perf_hooks", shim("perf_hooks.ts")),
    builtin("tty", shim("tty.ts")),
    builtin("stream", shim("stream.ts")),
    builtin("worker_threads", shim("worker_threads.ts")),
    builtin("child_process", shim("child_process.ts")),
    builtin("async_hooks", shim("async_hooks.ts")),
    builtin("console", shim("console.ts")),
    builtin("(http|vm|assert|assert\\/strict|test)", shim("unavailable.ts")),
    { find: /^bun:ffi$/, replacement: shim("unavailable.ts") },
    // npm polyfills (dependencies of demo/package.json; an app installs its own).
    builtin("path", requireFromHere.resolve("path-browserify")),
    builtin("events", requireFromHere.resolve("events/events.js")),
    builtin("buffer", requireFromHere.resolve("buffer/index.js")),
  ]
}
