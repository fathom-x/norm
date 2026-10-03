import type { Message, Part } from "@opencode-ai/sdk/v2"
import { notice } from "./norm-notification"

// norm: messages sent while a reply is in progress wait behind it, marked
// QUEUED (routes/session/index.tsx). The session view offers "press esc to
// send immediately" under them, and esc (component/prompt) interrupts the
// reply and has the server answer them now (POST /session/:id/send_queued).

/** How many user messages wait behind the reply in progress — the same
 * rule as the QUEUED badge: user messages after the newest unfinished
 * assistant message that follows the last finished one. Background
 * notifications (hidden messages from a monitor) are not the user's and do
 * not count: with only those waiting, esc still interrupts. */
export function queuedCount(
  messages: readonly Message[],
  parts: Readonly<Record<string, readonly Part[] | undefined>> = {},
): number {
  const completed = messages.findLastIndex((message) => message.role === "assistant" && !!message.time.completed)
  const pending = messages.findLastIndex(
    (message, index) => index > completed && message.role === "assistant" && !message.time.completed,
  )
  if (pending === -1) return 0
  return messages
    .slice(pending + 1)
    .filter((message) => message.role === "user" && notice(parts[message.id]) === undefined).length
}

type Post = (options: { url: string; path: Record<string, string> }) => Promise<unknown>

/** Interrupt the reply in progress and answer the queued messages now. The
 * endpoint is norm's own, so the generated SDK has no method for it; this
 * goes through the SDK's underlying HTTP client, which carries the same
 * base URL, directory and auth headers as every generated call. */
export function sendQueued(client: unknown, sessionID: string) {
  const http = (client as { client: { post: Post } }).client
  return http.post({ url: "/session/{sessionID}/send_queued", path: { sessionID } })
}
