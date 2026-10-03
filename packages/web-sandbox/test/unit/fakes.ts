// Test doubles: a scripted provider and a peer that records what the hub sends.
import type { Peer, StatusMessage } from "../../server/hub"
import type {
  AttachOptions,
  Attachment,
  FindOrCreateOptions,
  SandboxHandle,
  SandboxProvider,
  SandboxState,
} from "../../server/provider"

export const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms))

export async function until(check: () => boolean, ms = 2000, what = "condition") {
  const end = Date.now() + ms
  while (!check()) {
    if (Date.now() > end) throw new Error(`timed out waiting for ${what}`)
    await sleep(5)
  }
}

export interface FakeTerminal {
  attachments: AttachOptions[]
  input: Uint8Array[]
  sizes: Array<[number, number]>
  detached: number
  output(text: string): void
  exit(code: number | undefined): void
}

export class FakeProvider implements SandboxProvider {
  readonly name = "fake"
  states = new Map<string, SandboxState>()
  terminals = new Map<string, FakeTerminal>()
  calls: string[] = []
  /** Throw this from findOrCreate. */
  failWith: Error | undefined
  /** Delay findOrCreate by this many ms. */
  delayMs = 0

  terminal(sid: string): FakeTerminal {
    let terminal = this.terminals.get(sid)
    if (!terminal) {
      const attachments: AttachOptions[] = []
      terminal = {
        attachments,
        input: [],
        sizes: [],
        detached: 0,
        output: (text) => attachments.at(-1)?.onData(new TextEncoder().encode(text)),
        exit: (code) => attachments.at(-1)?.onExit(code),
      }
      this.terminals.set(sid, terminal)
    }
    return terminal
  }

  async findOrCreate(sid: string, options: FindOrCreateOptions): Promise<SandboxHandle> {
    this.calls.push(`findOrCreate ${sid}`)
    if (this.delayMs) await sleep(this.delayMs)
    if (this.failWith) throw this.failWith
    const state = this.states.get(sid) ?? "none"
    if (state === "none") {
      await options.admit()
      options.onPhase?.("creating")
    } else if (state === "paused") options.onPhase?.("resuming")
    this.states.set(sid, "running")
    const terminal = this.terminal(sid)
    return {
      attach: async (attach): Promise<Attachment> => {
        terminal.attachments.push(attach)
        terminal.sizes.push([attach.cols, attach.rows])
        return {
          write: (bytes) => void terminal.input.push(bytes),
          resize: (cols, rows) => void terminal.sizes.push([cols, rows]),
          detach: () => void (terminal.detached += 1),
        }
      },
    }
  }

  async reset(sid: string) {
    this.calls.push(`reset ${sid}`)
    this.states.delete(sid)
    this.terminals.delete(sid)
  }

  async pause(sid: string) {
    this.calls.push(`pause ${sid}`)
    if (this.states.get(sid) === "running") this.states.set(sid, "paused")
  }

  async status(sid: string) {
    return this.states.get(sid) ?? "none"
  }

  async count() {
    return [...this.states.values()].filter((state) => state === "running").length
  }
}

export class FakePeer implements Peer {
  texts: string[] = []
  binaries: Uint8Array[] = []
  closed: { code: number; reason: string } | undefined

  sendText(text: string) {
    this.texts.push(text)
  }
  sendBinary(bytes: Uint8Array) {
    this.binaries.push(bytes)
  }
  close(code: number, reason: string) {
    this.closed = { code, reason }
  }
  get statuses(): StatusMessage[] {
    return this.texts.map((text) => JSON.parse(text) as StatusMessage)
  }
  get phases() {
    return this.statuses.map((status) => status.phase)
  }
  get output() {
    return this.binaries.map((bytes) => new TextDecoder().decode(bytes)).join("")
  }
}
