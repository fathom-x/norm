import { fileURLToPath } from "node:url"
import { defineConfig } from "vite"

const web = fileURLToPath(new URL("./web", import.meta.url))

// Two plain pages, served by the broker (server/): the landing page at / and
// the terminal at /sandbox/. `vite build` writes them to dist/web.
export default defineConfig({
  root: web,
  base: "/",
  build: {
    outDir: fileURLToPath(new URL("./dist/web", import.meta.url)),
    emptyOutDir: true,
    target: "es2022",
    rollupOptions: {
      input: {
        main: `${web}/index.html`,
        sandbox: `${web}/sandbox/index.html`,
      },
    },
  },
})
