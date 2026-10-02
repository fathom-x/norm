export * as NormCompaction from "./norm-compaction"

import path from "path"
import fs from "fs/promises"
import { Global } from "./global"

// The model norm compacts conversations with (`/compaction-model`), as
// "provider/model". Unset means opencode's default: the conversation's own
// model — one call at nearly its full context window, so often a
// conversation's priciest. The server-side norm plugin applies it as
// `agent.compaction.model` unless the user's own config sets one.

export function file() {
  return path.join(Global.Path.data, "norm-compaction.json")
}

export async function get(): Promise<string | undefined> {
  const parsed = await fs
    .readFile(file(), "utf8")
    .then((text) => JSON.parse(text))
    .catch(() => undefined)
  return typeof parsed?.model === "string" && parsed.model.includes("/") ? parsed.model : undefined
}

export async function set(model: string | undefined): Promise<void> {
  if (!model) {
    await fs.rm(file(), { force: true })
    return
  }
  await fs.mkdir(path.dirname(file()), { recursive: true })
  await fs.writeFile(file(), JSON.stringify({ model }, null, 2))
}
