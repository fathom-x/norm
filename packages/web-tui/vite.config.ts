import path from "node:path"
import { fileURLToPath } from "node:url"
import { defineConfig } from "vite"
import solidPlugin from "vite-plugin-solid"
import { browserBuild } from "./build/browser-build"

const opencode = fileURLToPath(new URL("../opencode/src", import.meta.url))

// One build path: `vite build` bundles the page and the core worker into
// dist/ (`vite preview` serves it); `vite` serves the same graph for
// development. See README.md.
export default defineConfig(({ command }) => ({
  plugins: [browserBuild(), solidPlugin()],
  worker: {
    format: "es",
    plugins: () => [browserBuild()],
  },
  resolve: {
    // opencode's own `@/…` path alias (packages/opencode/tsconfig.json).
    alias: [{ find: /^@\//, replacement: `${opencode}${path.sep}` }],
    conditions: ["browser", "module", "import", "default"],
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
