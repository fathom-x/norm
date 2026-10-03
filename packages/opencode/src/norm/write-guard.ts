import path from "path"
import type { SessionV1 } from "@opencode-ai/core/v1/session"

// norm: the write tool replaces a whole file, so overwriting one the model
// never looked at — or that changed on disk since it last did (the user
// saved it, a formatter ran, another session wrote it) — throws that work
// away. write.txt has always said "you MUST read it first … this tool will
// fail", but nothing enforced it. This does, from the session's own history
// (so it survives a restart): the last time this session read, edited,
// wrote or patched the file, against the file's modification time. edit
// doesn't need it — it must match the file's exact text to change anything.

/** Modifications this soon after the session's last touch are taken to be
 * that touch's own (a formatter running after a write, clock rounding). */
const SLACK_MS = 1000

/** When this session last read or wrote `filepath` (ms epoch), from its
 * completed read / edit / write / apply_patch tool calls. */
export function lastTouched(messages: readonly SessionV1.WithParts[], filepath: string, directory: string) {
  const target = path.resolve(directory, filepath)
  const same = (value: unknown) => typeof value === "string" && path.resolve(directory, value) === target
  let latest: number | undefined
  for (const message of messages) {
    for (const part of message.parts) {
      if (part.type !== "tool" || part.state.status !== "completed") continue
      const input = part.state.input as Record<string, unknown>
      const files = (part.state.metadata as { files?: { filePath?: unknown; movePath?: unknown }[] } | undefined)
        ?.files
      const hit =
        ((part.tool === "read" || part.tool === "edit" || part.tool === "write") && same(input.filePath)) ||
        (part.tool === "apply_patch" &&
          Array.isArray(files) &&
          files.some((file) => same(file.filePath) || same(file.movePath)))
      if (!hit) continue
      const end = part.state.time.end
      if (latest === undefined || end > latest) latest = end
    }
  }
  return latest
}

/** Why overwriting an existing file isn't allowed yet, or undefined if it is. */
export function refusal(input: {
  messages: readonly SessionV1.WithParts[]
  filepath: string
  directory: string
  modifiedMs: number
}): string | undefined {
  const touched = lastTouched(input.messages, input.filepath, input.directory)
  if (touched === undefined)
    return `${input.filepath} already exists and hasn't been read in this session. Read it first, then write it (or use edit for a targeted change).`
  if (input.modifiedMs > touched + SLACK_MS)
    return `${input.filepath} has changed on disk since you last read it in this session. Read it again before overwriting it, so those changes aren't lost.`
  return undefined
}

export * as NormWriteGuard from "./write-guard"
