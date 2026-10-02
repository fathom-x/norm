# opentui-wasm

opentui (the TUI renderer under norm's `packages/tui`) running on a
**wasm32-wasi build of its Zig core**, so the real `@opentui/core` /
`@opentui/solid` can render into xterm.js in a browser tab, and so Bun can run
opentui's (and later norm's) test suites against that same wasm core.

This directory is self-contained. It is **not** wired into norm's build: it
carries a patch set against upstream opentui, a build script, browser shims, a
demo and a Playwright test.

| Path | What |
|---|---|
| `UPSTREAM` | upstream repo, pinned tag/commit (`v0.4.5`, matching norm's `@opentui/*` catalog pin), Zig version (0.15.2) |
| `patches/0001-*.patch` | Zig side: the `wasm32-wasi` build |
| `patches/0002-*.patch` | JS side: the wasm `FfiBackend`, browser runtime-assets, browser/wasm lib outputs |
| `patches/bun-ffi-structs@0.2.4.patch` | lets `bun-ffi-structs` (bundled into the core dist) take the wasm backend's `ptr` and 4-byte pointers |
| `scripts/build.sh` | clone → patch → fetch Zig → build wasm + `@opentui/core` + `@opentui/solid` → `dist/` |
| `src/boot.ts` | `bootOpenTUIWasm()`: process shim + instantiate the core + hook a terminal |
| `src/shims/` | browser stand-ins for the Node built-ins `@opentui/core` imports |
| `src/vite-aliases.ts` | the Vite `resolve.alias` list (built `dist/` or patched sources) |
| `demo/` | Vite page: xterm.js + a core scene (`?scene=core`) and a Solid scene (`?scene=solid`) |
| `test/demo.test.mjs` | Playwright (headless Chromium) checks against xterm's buffer; screenshots in `test/screenshots/` |

## Build and run

```bash
bash scripts/build.sh          # → dist/opentui.wasm, dist/core, dist/solid  (needs git, bun, node, curl)
npm test                       # builds demo/, runs test/demo.test.mjs
npm run demo                   # vite preview on :4173
OPENTUI_SOURCE=1 npm run demo:dev   # demo against .work/opentui sources (iterate on the patches)
```

`scripts/build.sh` keeps its scratch in `.work/` (`OPENTUI_WORK`, `OPENTUI_TOOLS`
override): the opentui checkout, the downloaded Zig, and git checkouts of the Zig
package dependencies when Zig's own fetcher cannot get through the proxy.

To change a patch: edit `.work/opentui` (branch `opentui-wasm`), commit, then
`git -C .work/opentui format-patch v0.4.5 -o ../../patches/`.

## Using it from an app (what norm's web host should do)

```ts
import { Terminal } from "@xterm/xterm"
import wasmUrl from "<dist>/opentui.wasm?url"
import { bootOpenTUIWasm } from "<this package>/src/boot"

const term = new Terminal({ cols: 120, rows: 36 })
term.open(el)

// 1. install `process`/`Buffer`, instantiate the core, wire the terminal
const host = await bootOpenTUIWasm({ wasm: fetch(wasmUrl), terminal: term })

// 2. only now import opentui (dynamic import: module init reads `process`
//    and picks the FFI backend from globalThis.__OPENTUI_WASM__)
const { render } = await import("@opentui/solid")
await render(() => <App />, { exitOnCtrlC: false, useKittyKeyboard: null })
```

* Output: the core's `StdoutOutput` writes rendered frames to fd 1; the WASI
  shim hands those bytes to `terminal.write`. JS-side writes
  (`process.stdout.write`) go to the same terminal.
* Input: `terminal.onData` → `process.stdin.push()` → opentui's own stdin
  parser (`CliRenderer` listens on `process.stdin` "data"). xterm.js answers
  opentui's capability queries (DA1/DSR/...) through the same path.
* Resize: `terminal.onResize` updates `process.stdout.columns/rows` and emits
  `SIGWINCH` on `process`, which `CliRenderer` already handles.
* Lower level: `@opentui/core/wasm` exports `instantiateOpenTUIWasm(source,
  { stdout, stderr, env })` / `instantiateOpenTUIWasmSync` and
  `createWasmBackend`; set `globalThis.__OPENTUI_WASM__` to the instance before
  importing `@opentui/core`.

Bundler setup (Vite): `opentuiWasmAliases({ dist })` from `src/vite-aliases.ts`
maps

* `@opentui/core` → `dist/core/index.browser.js` (also selected by the
  `browser` export condition), `@opentui/core/wasm` → `dist/core/wasm.js`,
  `@opentui/core/testing` → a stub, `@opentui/solid` → `dist/solid/index.js`;
* `fs`, `fs/promises`, `os`, `url`, `module`, `util`, `perf_hooks`, `tty`,
  `stream`, `worker_threads`, `child_process`, `async_hooks`, `console`
  (with and without `node:`) → `src/shims/*`; `http`, `vm`, `assert`, `test`,
  `bun:ffi` → a throwing stub;
* `path` → `path-browserify`, `events` → `events`, `buffer` → `buffer`;
* the built `@opentui/solid`'s bare deps (`solid-js/dist/solid.js`,
  `solid-js/store`, `entities`) → the app's copies, so there is one solid-js.

and the Solid JSX transform is `vite-plugin-solid` with
`{ solid: { generate: "universal", moduleName: "@opentui/solid" } }` (the same
options `@opentui/solid`'s Bun plugin uses).

## Bun: `OPENTUI_BACKEND=wasm`

The same backend runs under Bun: `OPENTUI_BACKEND=wasm` makes `platform/ffi.ts`
synchronously instantiate `opentui.wasm` (from `OPENTUI_WASM_PATH`, next to the
bundle, or the in-tree Zig output) with fd 1/2 on `process.stdout/stderr`.
`createTestRenderer` suites run unchanged:

```bash
cd .work/opentui/packages/core && OPENTUI_BACKEND=wasm bun test
```

## How the wasm core differs from the native one

* **Compiled out:** miniaudio (audio API reports no device), the
  `BufferedBackend` render thread (single-threaded build), C++ exceptions/RTTI
  in yoga. No filesystem: `TextBuffer.loadFile` and the debug dump helpers fail.
* **Fixed memory:** initial == max (256 MiB, `-Dwasm-memory-mb`), so
  `memory.buffer` never changes and the long-lived typed-array views over
  `OptimizedBuffer` planes never detach. The core cannot use more than that.
* **`ptr()` copies:** JS buffers are staged in linear memory per call, copied
  back after the call when passed directly as arguments (out-parameters), and
  released when the synchronous turn ends. Native code must not keep a JS
  pointer past the call; the one place that did (text-buffer "borrowed" memory)
  copies on wasm. `toArrayBuffer` returns a copy; code that needs a live alias
  uses the new `toNativeView`.
* **Callbacks:** each `createCallback` instantiates a 1-function trampoline
  module and puts its export in the core's function table (made growable at
  load time; Zig's linker has no `--growable-table`).
