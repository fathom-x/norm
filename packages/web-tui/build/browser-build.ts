// Vite plugins that let opencode's core (written for Bun/Node) bundle for a
// browser Web Worker. Three mechanisms, in order of preference:
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
import { builtinModules } from "node:module"
import path from "node:path"
import { fileURLToPath } from "node:url"
import type { Plugin } from "vite"

const root = path.resolve(fileURLToPath(new URL("..", import.meta.url)))
const packages = path.resolve(root, "..")
const shim = (file: string) => path.join(root, "src/shims", file)

/** core module → browser twin (absolute paths, both .ts). */
export const TWINS: Record<string, string> = Object.fromEntries(
  [
    ["core/src/effect/app-node-platform.ts", "core/src/effect/app-browser-platform.ts"],
    ["core/src/cross-spawn-spawner.ts", "core/src/cross-spawn-spawner.browser.ts"],
    ["core/src/ripgrep.ts", "core/src/ripgrep.browser.ts"],
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
  "opencode-gitlab-auth": shim("inert-auth-plugins.ts"),
  "opencode-poe-auth": shim("inert-auth-plugins.ts"),
  "@effect/platform-node/NodeFileSystem": shim("effect-node-filesystem.ts"),
  "@effect/platform-node/NodePath": shim("effect-node-path.ts"),
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
])

/** Built-in names an npm package provides; the package wins when installed. */
const POLYFILLED = new Set(["string_decoder", "punycode"])

const SHIM_FILES = new Map(
  Object.entries(SHIMS)
    .filter(([, target]) => path.isAbsolute(target))
    .map(([name, target]) => [target, name]),
)

const builtins = new Set(builtinModules.filter((name) => !name.startsWith("_")))
const STUB = "\0browser-stub:"
const MISSING = "\0browser-absent:"

export function browserBuild(): Plugin {
  return {
    name: "norm-web:browser-build",
    enforce: "pre",
    async resolveId(source, importer, options) {
      const bare = source.startsWith("node:") ? source.slice(5) : source
      const target = SHIMS[bare]
      if (target) {
        if (path.isAbsolute(target)) return target
        // An npm polyfill: resolve it from this package, not the importer.
        return this.resolve(target, path.join(root, "package.json"), { ...options, skipSelf: true })
      }
      if (ABSENT.has(source)) return MISSING + source
      // Bun loads these as text (prompts, tool descriptions).
      if (/\.(md|txt)$/.test(source)) {
        const resolved = await this.resolve(source, importer, { ...options, skipSelf: true })
        return resolved && `${resolved.id}?raw`
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
      const name = SHIM_FILES.get(id)
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
