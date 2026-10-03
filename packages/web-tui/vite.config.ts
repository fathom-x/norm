import path from "node:path"
import { fileURLToPath } from "node:url"
import { defineConfig } from "vite"
import solidPlugin from "vite-plugin-solid"
import { browserBuild, OPENTUI_DIST } from "./build/browser-build"

const opencode = fileURLToPath(new URL("../opencode/src", import.meta.url))

// One build path: `vite build` bundles the page (the TUI on opentui's wasm
// core) and the core worker into dist/, `vite preview` serves it. See README.md.
export default defineConfig(({ command }) => ({
  plugins: [
    browserBuild({ thread: "main", opentuiDist: OPENTUI_DIST }),
    // The TUI's JSX targets opentui's universal renderer — the transform
    // @opentui/solid's own Bun plugin applies. (The page's own code has no JSX.)
    solidPlugin({ solid: { generate: "universal", moduleName: "@opentui/solid" } }),
  ],
  worker: {
    format: "es",
    plugins: () => [browserBuild({ thread: "worker" })],
    // Distinct names: the page and the worker share many modules, and equal
    // chunk names made their source maps overwrite each other.
    rollupOptions: { output: { chunkFileNames: "assets/worker-[name]-[hash].js" } },
  },
  // vite-plugin-solid compiles the TUI's JSX before esbuild strips types; a few
  // files still carry an @jsxImportSource pragma esbuild would warn about.
  esbuild: { logOverride: { "unsupported-jsx-comment": "silent" } },
  resolve: {
    // opencode's own `@/…` path alias (packages/opencode/tsconfig.json).
    alias: [{ find: /^@\//, replacement: `${opencode}${path.sep}` }],
    conditions: ["browser", "module", "import", "default"],
    // One solid-js for the TUI, @opentui/solid and its store.
    dedupe: ["solid-js"],
  },
  define: {
    // Node's `global`. A production build would also inline `process.env` as
    // `{}`; the core reads its flags from it at runtime, so point it at the
    // process shim. (The dev server leaves `process.env` alone, and its own
    // client reads the define before any shim could run.)
    global: "globalThis",
    ...(command === "build" && {
      "process.env": "globalThis.process.env",
      "global.process.env": "globalThis.process.env",
      "globalThis.process.env": "globalThis.process.env",
    }),
  },
  optimizeDeps: {
    // Ships its own .wasm next to the module; pre-bundling would lose it.
    exclude: ["@sqlite.org/sqlite-wasm"],
  },
  server: { port: 4173, host: "127.0.0.1" },
  preview: { port: 4173, host: "127.0.0.1" },
  build: {
    target: "esnext",
    sourcemap: true,
    chunkSizeWarningLimit: 20_000,
  },
}))
