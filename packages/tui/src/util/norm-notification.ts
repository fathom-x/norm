import type { Part } from "@opencode-ai/sdk/v2"

// norm: background work (a monitor, a background command) reaches the model
// as a user message whose text is hidden from the transcript, a
// <task-notification> block. Without this the session view would show
// nothing for it, and the model would seem to start talking unprompted.

const OPEN = "<task-notification>"

function tag(text: string, name: string) {
  return text.match(new RegExp(`<${name}>([\\s\\S]*?)</${name}>`))?.[1]
}

/** One line for a message that carries only a background notification, or
 * nothing for any other message. */
export function notice(parts: readonly Part[] | undefined): string | undefined {
  if (!parts || parts.some((part) => part.type === "text" && !part.synthetic && part.text.trim())) return
  const text = parts.flatMap((part) => (part.type === "text" && part.text.startsWith(OPEN) ? [part.text] : []))[0]
  if (!text) return
  const summary = tag(text, "summary")?.trim()
  if (!summary) return
  const lines = tag(text, "event")
    ?.split("\n")
    .filter((line) => line.trim())
  if (!lines?.length) return summary
  return `${summary}: ${lines[0].trim()}${lines.length > 1 ? ` (+${lines.length - 1} more)` : ""}`
}
