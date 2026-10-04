import type { Part } from "@opencode-ai/sdk/v2"

// norm: wakeups, cron jobs, monitors and background commands reach the model
// as user-role messages that the user did not write. The chat history shows
// what the human typed and nothing else, so these render as nothing (their
// text parts are `synthetic`, which the session view already skips) and must
// not be treated as the user's in other places either.

/** Whether a user-role message is made only of text the user never saw:
 * nothing typed, attached or otherwise asked for by them. */
export function agentWritten(parts: readonly Part[] | undefined): boolean {
  if (!parts || parts.length === 0) return false
  return parts.every((part) => part.type === "text" && part.synthetic === true)
}
