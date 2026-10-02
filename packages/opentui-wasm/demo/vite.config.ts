import { defineConfig } from "vite"
import solid from "vite-plugin-solid"
import path from "node:path"
import { fileURLToPath } from "node:url"
import { opentuiWasmAliases } from "../src/vite-aliases"

const here = path.dirname(fileURLToPath(import.meta.url))
// Default: the built packages in ../dist (scripts/build.sh). OPENTUI_SOURCE=1
// builds against the patched checkout's TypeScript instead (OPENTUI_ROOT,
// default ../.work/opentui), for iterating on the patches.
const fromSource = process.env.OPENTUI_SOURCE === "1"
const opentuiRoot = process.env.OPENTUI_ROOT ?? path.resolve(here, "../.work/opentui")
const dist = path.resolve(here, "../dist")

export default defineConfig({
  root: here,
  base: "./",
  plugins: [
    // Same transform @opentui/solid's own bun plugin applies.
    solid({ solid: { generate: "universal", moduleName: "@opentui/solid" } }),
  ],
  resolve: {
    alias: opentuiWasmAliases(fromSource ? { opentuiRoot } : { dist }),
    dedupe: ["solid-js"],
    conditions: ["browser", "import", "module", "default"],
  },
  define: {
    // @opentui/core reads these at module scope.
    "process.env.NODE_ENV": JSON.stringify("production"),
  },
  build: {
    target: "es2022",
    outDir: "dist",
    emptyOutDir: true,
    sourcemap: true,
    chunkSizeWarningLimit: 4096,
  },
  esbuild: { target: "es2022" },
  optimizeDeps: { esbuildOptions: { target: "es2022" } },
  server: { fs: { allow: [path.resolve(here, ".."), opentuiRoot] } },
})
