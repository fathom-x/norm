// The slice of Bun the TUI uses on the page (main thread only — the worker
// build stubs `bun`): `Bun.file(path).text()/json()`, `Bun.write`,
// `Bun.stringWidth`, and the `bun` module's file-URL helpers. Files go
// through ZenFS, i.e. the worker's tree mounted over a port (main-vfs.ts).
import { fs } from "@zenfs/core"
import stringWidth from "string-width"
import { fileURLToPath, pathToFileURL } from "./url"

export { fileURLToPath, pathToFileURL }

function file(path: string) {
  return {
    text: () => fs.promises.readFile(path, "utf8"),
    json: async () => JSON.parse(await fs.promises.readFile(path, "utf8")),
    arrayBuffer: async () => {
      const bytes = await fs.promises.readFile(path)
      return bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength)
    },
    bytes: async () => new Uint8Array(await fs.promises.readFile(path)),
    exists: () =>
      fs.promises.stat(path).then(
        (stat) => stat.isFile(),
        () => false,
      ),
  }
}

async function write(path: string, data: string | Uint8Array | ArrayBuffer) {
  const bytes = typeof data === "string" ? data : new Uint8Array(data instanceof ArrayBuffer ? data : data.buffer)
  await fs.promises.writeFile(path, bytes)
  return typeof bytes === "string" ? new TextEncoder().encode(bytes).length : bytes.byteLength
}

export const Bun = {
  version: "1.3.14",
  file,
  write,
  stringWidth: (text: string) => stringWidth(text),
  env: globalThis.process?.env ?? {},
}

export function installBunGlobal() {
  const scope = globalThis as Record<string, unknown>
  scope.Bun ??= Object.assign(Bun, { env: globalThis.process?.env ?? {} })
}

export default Bun
