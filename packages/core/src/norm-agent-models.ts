export * as NormAgentModels from "./norm-agent-models"

import path from "path"
import fs from "fs/promises"
import { Global } from "./global"

// The models norm runs its housekeeping calls on, chosen with
// `/compaction-model` and `/title-model`, each stored as "provider/model".
// The server-side norm plugin applies them as `agent.<name>.model` unless the
// user's own config sets one.
//
// - compaction: unset means opencode's default, the conversation's own model
//   (one call at nearly its full context window — often its priciest).
// - title: unset means automatic: the marketplace's cheapest model. The
//   CONVERSATION value opts back into opencode's default, the chat model.

export type Agent = "compaction" | "title"

/** Title only: title with the conversation's own model. */
export const CONVERSATION = "conversation"

export function file(agent: Agent) {
  // compaction's name predates the title setting; kept so a stored choice survives.
  return path.join(Global.Path.data, agent === "compaction" ? "norm-compaction.json" : `norm-${agent}-model.json`)
}

export async function get(agent: Agent): Promise<string | undefined> {
  const parsed = await fs
    .readFile(file(agent), "utf8")
    .then((text) => JSON.parse(text))
    .catch(() => undefined)
  const model = parsed?.model
  if (typeof model !== "string") return undefined
  if (model.includes("/")) return model
  return agent === "title" && model === CONVERSATION ? model : undefined
}

export async function set(agent: Agent, model: string | undefined): Promise<void> {
  if (!model) {
    await fs.rm(file(agent), { force: true })
    return
  }
  await fs.mkdir(path.dirname(file(agent)), { recursive: true })
  await fs.writeFile(file(agent), JSON.stringify({ model }, null, 2))
}
