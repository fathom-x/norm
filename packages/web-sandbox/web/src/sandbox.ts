// /sandbox/: the visitor's sandbox terminal. xterm.js on the page, the bytes
// over one WebSocket to the broker (server/hub.ts has the protocol), which
// pipes them to a PTY in the visitor's own sandbox.
import { FitAddon } from "@xterm/addon-fit"
import { WebLinksAddon } from "@xterm/addon-web-links"
import { Terminal } from "@xterm/xterm"

type Phase = "creating" | "resuming" | "starting" | "ready" | "exited" | "replaced" | "error"
interface StatusMessage {
  type: "status"
  phase: Phase
  message?: string
  code?: number
}

// Close codes from the broker (server/hub.ts CLOSE).
const CLOSE_PROTOCOL = 4000
const CLOSE_REPLACED = 4001
const CLOSE_RESET = 4002
const CLOSE_ERROR = 4003
const CLOSE_EXITED = 4004

const container = document.getElementById("terminal")!
const overlay = document.getElementById("overlay")!
const overlayText = document.getElementById("overlay-text")!
const overlayDetail = document.getElementById("overlay-detail")!
const overlayAction = document.getElementById("overlay-action") as HTMLButtonElement
const startOver = document.getElementById("start-over") as HTMLButtonElement

const term = new Terminal({
  allowProposedApi: true,
  cursorBlink: false,
  fontFamily: '"JetBrains Mono", ui-monospace, SFMono-Regular, Menlo, Consolas, monospace',
  fontSize: 14,
  scrollback: 5000,
  theme: { background: "#0a0a0a" },
})
const fit = new FitAddon()
term.loadAddon(fit)
// Links (norm prints the Overpay login URL) open in a new tab that cannot
// reach back into this one.
term.loadAddon(
  new WebLinksAddon((event, uri) => {
    event.preventDefault()
    window.open(uri, "_blank", "noopener,noreferrer")
  }),
)
term.open(container)
fit.fit()
term.focus()

const refit = () => {
  try {
    fit.fit()
  } catch {
    // Not laid out yet.
  }
}
new ResizeObserver(refit).observe(container)
window.addEventListener("resize", refit)
container.addEventListener("pointerdown", () => term.focus())

let socket: WebSocket | undefined
let ready = false
/** Set while we close the socket ourselves (start over, take over). */
let intentional = false
let attempt = 0
let retryTimer: ReturnType<typeof setTimeout> | undefined
let lastStatus: StatusMessage | undefined

Object.assign(window, {
  __sandbox: {
    term,
    get socket() {
      return socket
    },
  },
})

const encoder = new TextEncoder()
function sendBytes(bytes: Uint8Array) {
  if (socket?.readyState === WebSocket.OPEN) socket.send(bytes)
}
function sendControl(message: Record<string, unknown>) {
  if (socket?.readyState === WebSocket.OPEN) socket.send(JSON.stringify(message))
}

term.onData((data) => sendBytes(encoder.encode(data)))
// onBinary: bytes xterm produces as a latin-1 string (some mouse reports).
term.onBinary((data) => sendBytes(Uint8Array.from(data, (char) => char.charCodeAt(0) & 0xff)))
term.onResize(({ cols, rows }) => sendControl({ type: "resize", cols, rows }))

function show(text: string, detail = "", options: { kind?: "info" | "error"; action?: string; onAction?: () => void } = {}) {
  overlay.hidden = false
  overlay.dataset.kind = options.kind ?? "info"
  overlayText.textContent = text
  overlayDetail.textContent = detail
  overlayAction.hidden = !options.action
  overlayAction.textContent = options.action ?? ""
  overlayAction.onclick = options.onAction ?? null
  if (options.action) overlayAction.focus()
}
function hide() {
  overlay.hidden = true
  term.focus()
}

const PHASE_TEXT: Partial<Record<Phase, [string, string]>> = {
  creating: ["Creating your sandbox…", "A private Linux machine, just for you. This takes a few seconds."],
  resuming: ["Resuming your sandbox…", "Picking up where you left off."],
  starting: ["Starting norm…", ""],
}

function onStatus(status: StatusMessage) {
  lastStatus = status
  const text = PHASE_TEXT[status.phase]
  if (text) return show(...text)
  if (status.phase === "ready") {
    ready = true
    attempt = 0
    hide()
    return
  }
  // exited / replaced / error: the close that follows decides what to offer.
}

async function ensureSession() {
  // Issues the session cookie if this visit has none (e.g. it expired).
  const response = await fetch("/api/session", { cache: "no-store" })
  if (!response.ok) throw new Error(`session: HTTP ${response.status}`)
  return (await response.json()) as { exists: boolean; state: string }
}

async function connect() {
  clearTimeout(retryTimer)
  intentional = false
  ready = false
  lastStatus = undefined
  show(attempt ? "Reconnecting…" : "Connecting…")
  try {
    await ensureSession()
  } catch {
    return scheduleReconnect()
  }
  const ws = new WebSocket(`${location.protocol === "https:" ? "wss" : "ws"}://${location.host}/api/tty`)
  ws.binaryType = "arraybuffer"
  socket = ws
  ws.onopen = () => {
    // A fresh screen for every connection: a resumed program redraws it (and
    // the local stand-in replays its recent output).
    term.reset()
    ws.send(JSON.stringify({ type: "hello", cols: term.cols, rows: term.rows }))
  }
  ws.onmessage = (event) => {
    if (socket !== ws) return
    if (typeof event.data === "string") {
      try {
        const message = JSON.parse(event.data) as StatusMessage
        if (message.type === "status") onStatus(message)
      } catch {
        // Not ours.
      }
      return
    }
    term.write(new Uint8Array(event.data as ArrayBuffer))
  }
  ws.onclose = (event) => {
    if (socket !== ws) return
    socket = undefined
    if (intentional) return
    switch (event.code) {
      case CLOSE_REPLACED:
        return show("Open in another tab", "This sandbox is now in use from another tab or window.", {
          action: "Use it here",
          onAction: () => void connect(),
        })
      case CLOSE_RESET:
        return show("Your sandbox was deleted", "It was started over from another tab.", {
          action: "Start a new one",
          onAction: () => void connect(),
        })
      case CLOSE_EXITED:
        return show(
          "norm exited",
          lastStatus?.code !== undefined ? `Exit code ${lastStatus.code}.` : "",
          { action: "Restart", onAction: () => void connect() },
        )
      case CLOSE_ERROR:
      case CLOSE_PROTOCOL:
        return show("Could not connect", lastStatus?.message ?? "Something went wrong.", {
          kind: "error",
          action: "Retry",
          onAction: () => {
            attempt = 0
            void connect()
          },
        })
      default:
        return scheduleReconnect()
    }
  }
}

function scheduleReconnect() {
  const delay = Math.min(15_000, 500 * 2 ** attempt) + Math.random() * 250
  attempt += 1
  show("Reconnecting…", ready ? "The connection dropped." : `Trying again in ${Math.ceil(delay / 1000)} s.`, {
    action: "Now",
    onAction: () => void connect(),
  })
  clearTimeout(retryTimer)
  retryTimer = setTimeout(() => void connect(), delay)
}

startOver.addEventListener("click", async () => {
  const ok = window.confirm(
    "Start over?\n\nThis deletes your sandbox — the wallet, your conversations and files — and starts a fresh one.",
  )
  if (!ok) return
  intentional = true
  clearTimeout(retryTimer)
  socket?.close(1000, "start over")
  socket = undefined
  show("Starting over…", "Deleting your sandbox.")
  try {
    const response = await fetch("/api/reset", { method: "POST" })
    if (!response.ok) throw new Error(`HTTP ${response.status}`)
  } catch (error) {
    return show("Could not start over", (error as Error).message, {
      kind: "error",
      action: "Back to the terminal",
      onAction: () => void connect(),
    })
  }
  attempt = 0
  term.reset()
  void connect()
})

void connect()
