// The Vite plugin that lets opencode (written for Bun/Node) bundle for a
// browser: the core server in a Web Worker (`thread: "worker"`) and the TUI
// on the page (`thread: "main"`, which adds opentui on its wasm core —
// packages/opentui-wasm). Three mechanisms, in order of preference:
//
// 1. Browser twins — whole core modules replaced by a browser sibling with the
//    same exports (TWINS). Used where the module *is* the platform seam.
// 2. Node built-in shims — `fs`, `path`, `process`, `crypto`, ... mapped to
//    browser implementations in src/shims (SHIMS) or to npm polyfills.
// 3. Stubs — every other Node built-in, and `bun` / `bun:*`, becomes a module
//    whose functions throw "<name> is not available in the browser build"
//    when called. Importing one is harmless; only calling it fails, and every
//    such call sits on a path the browser build turns off (servers, child
//    processes, native addons).
import { readFileSync } from "node:fs"
import { builtinModules } from "node:module"
import path from "node:path"
import { fileURLToPath } from "node:url"
import type { Plugin } from "vite"

const root = path.resolve(fileURLToPath(new URL("..", import.meta.url)))
const packages = path.resolve(root, "..")
const shim = (file: string) => path.join(root, "src/shims", file)
const opentuiShim = (file: string) => path.join(packages, "opentui-wasm/src/shims", file)

/** packages/opentui-wasm's build output (scripts/build.sh). */
export const OPENTUI_DIST = path.join(packages, "opentui-wasm/dist")

/** module → browser twin with the same exports (absolute paths). */
export const TWINS: Record<string, string> = Object.fromEntries(
  [
    ["core/src/effect/app-node-platform.ts", "core/src/effect/app-browser-platform.ts"],
    ["core/src/cross-spawn-spawner.ts", "core/src/cross-spawn-spawner.browser.ts"],
    ["core/src/ripgrep.ts", "core/src/ripgrep.browser.ts"],
    ["core/src/npm.ts", "core/src/npm.browser.ts"],
    ["opencode/src/tool/shell.ts", "opencode/src/tool/shell.browser.ts"],
    ["opencode/src/tool/webfetch.txt", "opencode/src/tool/webfetch.browser.txt"],
  ].map(([from, to]) => [path.join(packages, from), path.join(packages, to)]),
)

/**
 * Node built-in (without `node:`), or a package that cannot work in a tab,
 * → browser implementation.
 */
export const SHIMS: Record<string, string> = {
  process: shim("process.ts"),
  os: shim("os.ts"),
  path: shim("path.ts"),
  "path/posix": shim("path.ts"),
  fs: shim("fs.ts"),
  "fs/promises": shim("fs-promises.ts"),
  url: shim("url.ts"),
  util: shim("util.ts"),
  crypto: shim("crypto.ts"),
  async_hooks: shim("async-hooks.ts"),
  "timers/promises": shim("timers-promises.ts"),
  tty: shim("tty.ts"),
  module: shim("module.ts"),
  diagnostics_channel: shim("diagnostics-channel.ts"),
  events: "events",
  buffer: "buffer",
  stream: "readable-stream",
  "@effect/platform-node": shim("effect-platform-node.ts"),
  open: shim("open.ts"),
  clipboardy: shim("clipboardy.ts"),
  "opencode-gitlab-auth": shim("inert-auth-plugins.ts"),
  "opencode-poe-auth": shim("inert-auth-plugins.ts"),
  "@effect/platform-node/NodeFileSystem": shim("effect-node-filesystem.ts"),
  "@effect/platform-node/NodePath": shim("effect-node-path.ts"),
}

/**
 * The page (main thread) also runs opentui and the TUI. These replace the
 * worker's mapping where the terminal needs more than a stub: opentui-wasm's
 * own shims for the built-ins its renderer touches (it owns `process` there —
 * see shims/process.ts), and a `Bun` stand-in for the TUI's Bun.file/write.
 */
export const MAIN_SHIMS: Record<string, string> = {
  perf_hooks: opentuiShim("perf_hooks.ts"),
  console: opentuiShim("console.ts"),
  worker_threads: opentuiShim("worker_threads.ts"),
  tty: opentuiShim("tty.ts"),
  child_process: opentuiShim("child_process.ts"),
  bun: shim("bun.ts"),
  "@opentui/core/testing": opentuiShim("testing-unavailable.ts"),
  "@opentui/solid/runtime-plugin-support": shim("runtime-plugin-support.ts"),
  "@opentui/solid/runtime-plugin-support/configure": shim("runtime-plugin-support.ts"),
}

/**
 * Modules a native build generates or that only work natively. Evaluating
 * one throws, so the lazy `import()` chain that reaches it rejects — and
 * every such chain is already error-handled (the feature reports itself
 * unavailable).
 */
export const ABSENT = new Set([
  "opencode-web-ui.gen.ts",
  // Image resizing (node build of photon reads its .wasm from disk); the
  // image module already degrades to "resizer unavailable".
  "@silvia-odwyer/photon-node",
  // Cloud-provider credential chains (files, metadata servers, child
  // processes). norm offers only the Overpay provider, and these plugins
  // load them lazily for their own providers.
  "@aws-sdk/credential-providers",
  "google-auth-library",
  // opentui's native libraries (the page uses the wasm core).
  "@opentui/core-darwin-arm64",
  "@opentui/core-darwin-x64",
  "@opentui/core-linux-arm64",
  "@opentui/core-linux-arm64-musl",
  "@opentui/core-linux-x64",
  "@opentui/core-linux-x64-musl",
  "@opentui/core-win32-arm64",
  "@opentui/core-win32-x64",
])

/** Built-in names an npm package provides; the package wins when installed. */
const POLYFILLED = new Set(["string_decoder", "punycode"])

const builtins = new Set(builtinModules.filter((name) => !name.startsWith("_")))
const STUB = "\0browser-stub:"
const MISSING = "\0browser-absent:"

export interface BrowserBuildOptions {
  thread: "worker" | "main"
  /** opentui-wasm's dist/ (main thread only). */
  opentuiDist?: string
}

export function browserBuild(options: BrowserBuildOptions): Plugin {
  const main = options.thread === "main"
  const shims = main ? { ...SHIMS, ...MAIN_SHIMS } : SHIMS
  const shimFiles = new Map(
    Object.entries(shims)
      .filter(([, target]) => path.isAbsolute(target) && target.startsWith(root))
      .map(([name, target]) => [target, name]),
  )
  const dist = options.opentuiDist ?? OPENTUI_DIST
  return {
    name: `norm-web:browser-build:${options.thread}`,
    enforce: "pre",
    async resolveId(source, importer, options) {
      const bare = source.startsWith("node:") ? source.slice(5) : source
      const target = shims[bare]
      if (target) {
        if (path.isAbsolute(target)) return target
        // An npm polyfill: resolve it from this package, not the importer.
        return this.resolve(target, path.join(root, "package.json"), { ...options, skipSelf: true })
      }
      if (ABSENT.has(source)) return MISSING + source
      if (main) {
        const opentui = opentuiPackage(dist, source)
        if (opentui) return opentui
        // The built opentui packages import their few runtime dependencies
        // (solid-js, entities, ...) by bare name: resolve them from this
        // package, so there is one solid-js on the page.
        if (importer?.startsWith(dist) && /^[@a-z]/.test(source) && !builtins.has(bare) && !source.startsWith("bun"))
          return this.resolve(source, path.join(root, "package.json"), { ...options, skipSelf: true })
      }
      // Bun loads these as text (prompts, tool descriptions).
      if (/\.(md|txt)$/.test(source)) {
        const resolved = await this.resolve(source, importer, { ...options, skipSelf: true })
        return resolved && `${TWINS[resolved.id] ?? resolved.id}?raw`
      }
      // Bun's `import x from "./a.wasm" with { type: "file" }` yields a path;
      // the closest browser equivalent is the asset's URL.
      if (source.endsWith(".wasm")) {
        const resolved = await this.resolve(source, importer, { ...options, skipSelf: true })
        return resolved && `${resolved.id}?url`
      }
      if (source === "bun" || source.startsWith("bun:")) return STUB + source
      if (builtins.has(bare)) {
        // A bare name an npm package provides (string_decoder, punycode, ...)
        // wins over the built-in, as browserify-style resolution does.
        if (!source.startsWith("node:") && POLYFILLED.has(bare)) {
          const resolved = await this.resolve(source, importer, { ...options, skipSelf: true })
          if (resolved && !resolved.id.startsWith("__vite-browser-external")) return resolved
        }
        return STUB + bare
      }
      const resolved = await this.resolve(source, importer, { ...options, skipSelf: true })
      const twin = resolved && TWINS[resolved.id]
      // The twin may import its original (types only today); never redirect those.
      if (twin && importer !== twin) return twin
      return resolved
    },
    // A shim implements what opencode calls; any other name a dependency
    // imports from it links to a throwing stub instead of failing the build.
    transform(code, id) {
      if (main && id.startsWith(dist)) return patchOpentuiDist(code)
      const name = shimFiles.get(id)
      if (!name) return
      return {
        code: `${code}\n${stubPrelude(name)}\nexport const $fallback = $stub`,
        map: null,
        syntheticNamedExports: "$fallback",
      }
    },
    async load(id) {
      // Links (any named import resolves through $fallback) but throws when
      // evaluated, so only the lazy import() that reaches it fails.
      if (id.startsWith(MISSING))
        return {
          code: `export const $fallback = {}\nthrow new Error(${JSON.stringify(`${id.slice(MISSING.length)} is not part of the browser build`)})`,
          syntheticNamedExports: "$fallback",
        }
      if (!id.startsWith(STUB)) return
      const name = id.slice(STUB.length)
      return { code: await stubModule(name), syntheticNamedExports: true }
    },
  }
}

// Two edits to the built opentui core for the page:
// - CliRenderer installs its own requestAnimationFrame on the global object
//   (Bun has none; callbacks then run inside its render loop). In a browser
//   the global *is* `window`, so that also captured xterm.js's frames and the
//   terminal's DOM stopped updating. The browser's own rAF serves the TUI's
//   few callers just as well, so the override is renamed away.
// - The tree-sitter parser loader builds `new URL(\`./${path}\`)`, which Vite
//   expands into a glob over the whole dist/core directory (every chunk, map
//   and .d.ts copied into the build). Highlighting assets are not served in
//   the browser yet, so the URL is left to runtime.
function patchOpentuiDist(code: string) {
  const patched = code
    .replace("global.requestAnimationFrame =", "global.__opentuiRequestAnimationFrame =")
    .replace("global.cancelAnimationFrame =", "global.__opentuiCancelAnimationFrame =")
    .replace("global.window.requestAnimationFrame = requestAnimationFrame;", "")
    .replace("new URL(`./${relativePath}`, import.meta.url)", "new URL(/* @vite-ignore */ `./${relativePath}`, import.meta.url)")
  if (patched === code) return
  return { code: patched, map: null }
}

// `@opentui/core[/x]` / `@opentui/solid[/x]` → the built package's file for
// that export, preferring the `browser` condition (the wasm-backed core).
function opentuiPackage(dist: string, source: string) {
  const match = /^@opentui\/(core|solid)(\/.*)?$/.exec(source)
  if (!match) return
  const dir = path.join(dist, match[1])
  const exports = readExports(dir)
  const entry = exports[`.${match[2] ?? ""}`]
  if (typeof entry === "string") return path.join(dir, entry)
  const target = entry && (entry.browser ?? entry.import ?? entry.default)
  if (typeof target === "string") return path.join(dir, target)
}

const exportsCache = new Map<string, Record<string, string | Record<string, string>>>()
function readExports(dir: string) {
  const cached = exportsCache.get(dir)
  if (cached) return cached
  const exports = JSON.parse(readFileSync(path.join(dir, "package.json"), "utf8")).exports ?? {}
  exportsCache.set(dir, exports)
  return exports
}

// Named exports come from the real Node module when this build runs on Node
// or Bun (so `import { spawn } from "child_process"` links in dev and build);
// anything else resolves through `syntheticNamedExports` on the default
// export, a Proxy that hands out throwing functions.
async function stubModule(name: string) {
  const real: Record<string, unknown> = name.startsWith("bun")
    ? {}
    : await import(`node:${name}`).catch(() => ({}))
  const names = Object.keys(real).filter((key) => key !== "default" && /^[A-Za-z_$][\w$]*$/.test(key))
  return [
    stubPrelude(name, constantsOf(real)),
    `export default $stub`,
    ...names.map((key) => `export const ${key} = $stub[${JSON.stringify(key)}]`),
  ].join("\n")
}

function stubPrelude(name: string, constants: Record<string, unknown> = {}) {
  return [
    `const $label = ${JSON.stringify(name)}`,
    `const $unavailable = (member) => { const fail = function () { throw new Error($label + (member ? "." + member : "") + " is not available in the browser build") }; return new Proxy(fail, { get: (target, key) => key === "prototype" ? target.prototype : key === Symbol.toPrimitive ? () => $label : $unavailable(String(key)), construct: () => fail() }) }`,
    `const $constants = ${JSON.stringify(constants)}`,
    `const $stub = new Proxy({}, { get: (_, key) => key === "__esModule" ? false : key === "then" ? undefined : key in $constants ? $constants[key] : $unavailable(String(key)) })`,
  ].join("\n")
}

// Plain data members (fs.constants, os.EOL, http.STATUS_CODES) are copied so
// reading them works; functions stay stubs.
function constantsOf(real: Record<string, unknown>) {
  return Object.fromEntries(
    Object.entries(real).filter(([, value]) => {
      if (value === null || typeof value === "function") return false
      if (typeof value !== "object") return true
      try {
        JSON.stringify(value)
        return Object.getPrototypeOf(value) === Object.prototype
      } catch {
        return false
      }
    }),
  )
}
