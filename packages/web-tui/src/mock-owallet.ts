// A scripted stand-in for owallet-web at http://owallet.internal, for tests
// and for demoing the page before the real WebAssembly owallet is plugged in
// (`?mock-owallet` on the page URL). It answers the routes norm uses — the
// /_mgmt setup calls, /health, /v1/status, /v1/models — and an
// OpenAI-compatible /v1/chat/completions that plays a fixed script of tool
// calls, chosen by a keyword in the user's message, then replies with what
// the last tool returned. One prompt thereby exercises the real session loop,
// tool registry and VFS end to end:
//
//   "search": grep TODO, then glob **/*.ts
//   "bash":   bash `ls` (the browser build refuses it)
//   anything else: write /workspace/notes/hello.md, then read it back
//
// Requests without tools (titles, summaries) get a short plain reply.
import type { Route } from "./fetch-router"

export const MOCK_FILE = "/workspace/notes/hello.md"
export const MOCK_CONTENT = "hello from the scripted model\n"
export const MOCK_VERSION = "0.1.99"

type Message = { role: string; content?: unknown; tool_calls?: unknown[] }
type ChatRequest = { model?: string; messages?: Message[]; tools?: unknown[]; stream?: boolean }

export const mockOwallet: Route = async (request) => {
  const { pathname } = new URL(request.url)
  if (pathname === "/health") return Response.json({ status: "ok", version: MOCK_VERSION })
  if (pathname === "/_mgmt/status")
    return Response.json({
      version: MOCK_VERSION,
      initialized: true,
      unlocked: true,
      wallet: { npub: "npub1mockwallet" },
      overpay_linked: true,
    })
  if (pathname === "/_mgmt/provider-key/create") return Response.json({ key: "owk_mock_browser_build" })
  if (pathname === "/v1/status") return Response.json({ key_can_spend: true, balance_cents: 500 })
  if (pathname === "/v1/models")
    return Response.json({
      object: "list",
      data: [
        {
          id: "default",
          name: "Scripted mock",
          context_length: 128_000,
          active: true,
          pricing: { input: 1, output: 2 },
        },
      ],
    })
  if (pathname === "/v1/chat/completions" && request.method === "POST")
    return complete((await request.json()) as ChatRequest)
  return Response.json({ error: { code: "not_found", message: `mock owallet has no ${pathname}` } }, { status: 404 })
}

type Call = { name: string; args: Record<string, unknown> }

const SCRIPTS: Record<string, Call[]> = {
  files: [
    { name: "write", args: { filePath: MOCK_FILE, content: MOCK_CONTENT } },
    { name: "read", args: { filePath: MOCK_FILE } },
  ],
  search: [
    { name: "grep", args: { pattern: "TODO" } },
    { name: "glob", args: { pattern: "**/*.ts" } },
  ],
  bash: [{ name: "bash", args: { command: "ls" } }],
}

function complete(body: ChatRequest) {
  const messages = body.messages ?? []
  const lastUser = messages.findLastIndex((message) => message.role === "user")
  const prompt = text(messages[lastUser]?.content)
  const script = SCRIPTS[Object.keys(SCRIPTS).find((name) => prompt.includes(name)) ?? "files"]
  const toolResults = messages.slice(lastUser + 1).filter((message) => message.role === "tool")
  const call = body.tools?.length ? script[toolResults.length] : undefined
  const step = call
    ? ({ type: "tool", ...call } as const)
    : ({
        type: "text",
        text: !body.tools?.length
          ? "Scripted tool run"
          : `Done. Last tool result: ${text(toolResults.at(-1)?.content).trim()}`,
      } as const)
  const id = `chatcmpl-mock-${Date.now()}`
  const created = Math.floor(Date.now() / 1000)
  const usage = { prompt_tokens: 42, completion_tokens: 7, total_tokens: 49, charged_cents: 1 }
  const message =
    step.type === "text"
      ? { role: "assistant", content: step.text }
      : {
          role: "assistant",
          content: null,
          tool_calls: [
            { id: `call_${step.name}`, type: "function", function: { name: step.name, arguments: JSON.stringify(step.args) } },
          ],
        }
  const finish = step.type === "text" ? "stop" : "tool_calls"
  if (!body.stream)
    return Response.json({
      id,
      object: "chat.completion",
      created,
      model: body.model ?? "default",
      choices: [{ index: 0, message, finish_reason: finish }],
      usage,
    })
  const chunk = (delta: unknown, finishReason: string | null, extra: object = {}) =>
    `data: ${JSON.stringify({ id, object: "chat.completion.chunk", created, model: body.model ?? "default", choices: [{ index: 0, delta, finish_reason: finishReason }], ...extra })}\n\n`
  const delta =
    step.type === "text"
      ? { role: "assistant", content: step.text }
      : { role: "assistant", tool_calls: message.tool_calls?.map((call, index) => ({ index, ...call })) }
  const encoder = new TextEncoder()
  return new Response(
    new ReadableStream({
      start(controller) {
        controller.enqueue(encoder.encode(chunk(delta, null)))
        controller.enqueue(encoder.encode(chunk({}, finish, { usage })))
        controller.enqueue(encoder.encode("data: [DONE]\n\n"))
        controller.close()
      },
    }),
    { headers: { "content-type": "text/event-stream", "cache-control": "no-cache" } },
  )
}

function text(content: unknown): string {
  if (typeof content === "string") return content
  if (Array.isArray(content))
    return content.map((part) => (part && typeof part === "object" && "text" in part ? String(part.text) : "")).join("")
  return ""
}
