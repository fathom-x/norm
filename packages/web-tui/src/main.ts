// norm in the browser — for now a debug panel over the core worker: the real
// TUI (packages/tui's `run()`) replaces it once opentui runs on WebAssembly.
// It boots the worker, creates and lists sessions, and reads/writes files in
// the demo workspace through the same RPC the TUI will use.
import "./shims/globals"
import { startCore, type Core } from "./core-client"
import { WORKSPACE } from "./env"

type Session = { id: string; title: string; directory: string; time: { created: number; updated: number } }

const params = new URLSearchParams(location.search)
const core = startCore({ mockOwallet: params.has("mock-owallet") })
const root = document.querySelector<HTMLElement>("#app")!

// For Playwright and the devtools console.
Object.assign(globalThis, { norm: { core, api } })

render()

async function api<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await core.fetch(path, {
    ...init,
    headers: { "content-type": "application/json", ...init?.headers },
  })
  const text = await response.text()
  if (!response.ok) throw new Error(`${init?.method ?? "GET"} ${path} → ${response.status}: ${text}`)
  return (text ? JSON.parse(text) : undefined) as T
}

function render() {
  root.innerHTML = `
    <header>
      <h1>norm <span>in the browser</span></h1>
      <p id="status" data-state="booting">Starting the core worker…</p>
    </header>
    <section>
      <h2>Sessions</h2>
      <div class="row">
        <button id="create" disabled>New session</button>
        <button id="refresh" disabled>Refresh</button>
      </div>
      <ul id="sessions"></ul>
    </section>
    <section>
      <h2>Files in ${WORKSPACE}</h2>
      <div class="row">
        <input id="path" value="README.md" aria-label="File path" />
        <button id="read" disabled>Read</button>
      </div>
      <pre id="content"></pre>
    </section>
    <section>
      <h2>Events</h2>
      <ol id="events"></ol>
    </section>
  `
  const status = root.querySelector<HTMLElement>("#status")!
  const buttons = [...root.querySelectorAll<HTMLButtonElement>("button")]
  core.ready.then(
    (boot) => {
      status.dataset.state = "ready"
      status.textContent = `Core ready — files: ${boot.storage}${boot.seeded ? " (demo workspace created)" : ""}`
      buttons.forEach((button) => (button.disabled = false))
      return listSessions()
    },
    (error: Error) => {
      status.dataset.state = "error"
      status.textContent = `Core failed to start: ${error.message}`
    },
  )
  root.querySelector("#create")!.addEventListener("click", () =>
    api<Session>("/session", { method: "POST", body: JSON.stringify({}) }).then(listSessions).catch(show),
  )
  root.querySelector("#refresh")!.addEventListener("click", () => listSessions().catch(show))
  root.querySelector("#read")!.addEventListener("click", () => {
    const path = root.querySelector<HTMLInputElement>("#path")!.value
    api<{ content: string }>(`/file/content?path=${encodeURIComponent(path)}`)
      .then((file) => (root.querySelector("#content")!.textContent = file.content))
      .catch(show)
  })
  const events = root.querySelector("#events")!
  core.onEvent((event) => {
    const item = document.createElement("li")
    item.textContent = `${event.payload.type} (${event.directory ?? "global"})`
    events.prepend(item)
    while (events.children.length > 20) events.lastChild?.remove()
  })
}

async function listSessions() {
  const sessions = await api<Session[]>("/session")
  const list = root.querySelector("#sessions")!
  list.replaceChildren(
    ...sessions.map((session) => {
      const item = document.createElement("li")
      item.dataset.id = session.id
      item.textContent = `${session.title} — ${session.id} — ${new Date(session.time.updated).toLocaleTimeString()}`
      return item
    }),
  )
  if (sessions.length === 0) list.innerHTML = "<li class=empty>No sessions yet</li>"
  return sessions
}

function show(error: unknown) {
  const status = root.querySelector<HTMLElement>("#status")!
  status.dataset.state = "error"
  status.textContent = error instanceof Error ? error.message : String(error)
}

export type { Core }
