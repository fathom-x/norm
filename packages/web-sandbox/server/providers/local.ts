// The local stand-in for a sandbox: a PTY on this machine (bun-pty) per
// visitor, running SANDBOX_COMMAND (`norm-demo` by default) in a private
// directory. For development and the browser tests — NOT isolation: the
// program runs as this server's user, with its filesystem and network.
//
// "Resume" here means the PTY outlives the WebSocket: it keeps running (and
// its output keeps landing in a bounded backlog) until the visitor resets, the
// program exits, or SANDBOX_IDLE_KILL_MS passes after a pause. A reattach
// replays the backlog, then nudges the size so a full-screen program redraws.
import { randomBytes } from "node:crypto"
import { chmodSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import path from "node:path"
import type { IPty } from "bun-pty"
import type { LocalConfig } from "../config"
import type { AttachOptions, Attachment, FindOrCreateOptions, SandboxHandle, SandboxProvider, SandboxState } from "../provider"
import { SID_PATTERN } from "../cookie"

/** Output kept for a reattach (bytes). */
export const BACKLOG_LIMIT = 256 * 1024

export type Spawn = (
  file: string,
  args: string[],
  options: { name: string; cols: number; rows: number; cwd: string; env: Record<string, string> },
) => IPty

interface Terminal {
  pty: IPty
  backlog: Uint8Array[]
  backlogBytes: number
  listener: AttachOptions | undefined
  exited: boolean
  idleTimer: ReturnType<typeof setTimeout> | undefined
}

export interface LocalProviderOptions extends LocalConfig {
  normOwalletEnv: string
  /** bun-pty's spawn; injectable for tests. */
  spawn?: Spawn
}

async function defaultSpawn(): Promise<Spawn> {
  const { spawn } = await import("bun-pty")
  return spawn
}

export class LocalProvider implements SandboxProvider {
  readonly name = "local"
  private readonly terminals = new Map<string, Terminal>()
  private spawnFn: Spawn | undefined

  constructor(private readonly options: LocalProviderOptions) {
    this.spawnFn = options.spawn
    mkdirSync(options.root, { recursive: true })
  }

  private dir(sid: string) {
    // The sid is a verified cookie value; refuse anything else as a path part.
    if (!SID_PATTERN.test(sid)) throw new Error("invalid sid")
    return path.join(this.options.root, sid)
  }

  /** The visitor's owallet password: random, made once, kept with their state. */
  private password(dir: string) {
    const file = path.join(dir, ".owallet-password")
    if (existsSync(file)) return readFileSync(file, "utf8").trim()
    const password = randomBytes(24).toString("base64url")
    writeFileSync(file, password, { mode: 0o600 })
    chmodSync(file, 0o600)
    return password
  }

  private environment(dir: string): Record<string, string> {
    const env: Record<string, string> = {}
    for (const name of this.options.passEnv) {
      const value = process.env[name]
      if (value !== undefined) env[name] = value
    }
    return {
      ...env,
      TERM: "xterm-256color",
      COLORTERM: "truecolor",
      NORM_OWALLET_ENV: this.options.normOwalletEnv,
      ...this.options.env,
      NORM_HOME: path.join(dir, "norm"),
      OWALLET_PASSWORD: this.password(dir),
    }
  }

  async findOrCreate(sid: string, options: FindOrCreateOptions): Promise<SandboxHandle> {
    const dir = this.dir(sid)
    const live = this.terminals.get(sid)
    if (!live || live.exited) {
      if (!existsSync(dir)) {
        await options.admit()
        options.onPhase?.("creating")
        mkdirSync(path.join(dir, "workspace"), { recursive: true })
        mkdirSync(path.join(dir, "norm"), { recursive: true })
      } else if (!live) options.onPhase?.("resuming")
    }
    if (live?.idleTimer) {
      clearTimeout(live.idleTimer)
      live.idleTimer = undefined
    }
    return { attach: (attach) => this.attach(sid, dir, attach) }
  }

  private async attach(sid: string, dir: string, options: AttachOptions): Promise<Attachment> {
    let terminal = this.terminals.get(sid)
    if (terminal && !terminal.exited) {
      // Reattach: the backlog first, then a size nudge for a redraw.
      terminal.listener = options
      for (const chunk of terminal.backlog) options.onData(chunk)
      const { cols, rows } = options
      terminal.pty.resize(cols, rows > 2 ? rows - 1 : rows + 1)
      terminal.pty.resize(cols, rows)
    } else {
      terminal = await this.start(sid, dir, options)
    }
    const current = terminal
    const decoder = new TextDecoder()
    return {
      write(bytes) {
        if (current.exited) return
        // bun-pty writes strings (as UTF-8): decode in streaming mode so a
        // character split across frames survives.
        current.pty.write(decoder.decode(bytes, { stream: true }))
      },
      resize(cols, rows) {
        if (!current.exited) current.pty.resize(cols, rows)
      },
      detach() {
        if (current.listener === options) current.listener = undefined
      },
    }
  }

  private async start(sid: string, dir: string, options: AttachOptions): Promise<Terminal> {
    this.spawnFn ??= await defaultSpawn()
    const pty = this.spawnFn(this.options.command, this.options.args, {
      name: "xterm-256color",
      cols: options.cols,
      rows: options.rows,
      cwd: path.join(dir, "workspace"),
      env: this.environment(dir),
    })
    const terminal: Terminal = {
      pty,
      backlog: [],
      backlogBytes: 0,
      listener: options,
      exited: false,
      idleTimer: undefined,
    }
    const encoder = new TextEncoder()
    pty.onData((text) => {
      const bytes = encoder.encode(text)
      terminal.backlog.push(bytes)
      terminal.backlogBytes += bytes.length
      while (terminal.backlogBytes > BACKLOG_LIMIT && terminal.backlog.length > 1)
        terminal.backlogBytes -= terminal.backlog.shift()!.length
      terminal.listener?.onData(bytes)
    })
    pty.onExit((event) => {
      terminal.exited = true
      if (terminal.idleTimer) clearTimeout(terminal.idleTimer)
      if (this.terminals.get(sid) === terminal) this.terminals.delete(sid)
      terminal.listener?.onExit(event.exitCode)
    })
    this.terminals.set(sid, terminal)
    return terminal
  }

  private kill(sid: string) {
    const terminal = this.terminals.get(sid)
    if (!terminal) return
    this.terminals.delete(sid)
    terminal.exited = true
    terminal.listener = undefined
    if (terminal.idleTimer) clearTimeout(terminal.idleTimer)
    try {
      terminal.pty.kill()
    } catch {
      // Already gone.
    }
  }

  async reset(sid: string) {
    const dir = this.dir(sid)
    this.kill(sid)
    rmSync(dir, { recursive: true, force: true })
  }

  async pause(sid: string) {
    const terminal = this.terminals.get(sid)
    if (!terminal || !this.options.idleKillMs) return
    if (terminal.idleTimer) clearTimeout(terminal.idleTimer)
    terminal.idleTimer = setTimeout(() => this.kill(sid), this.options.idleKillMs)
    terminal.idleTimer.unref?.()
  }

  async status(sid: string): Promise<SandboxState> {
    if (this.terminals.has(sid)) return "running"
    return existsSync(this.dir(sid)) ? "paused" : "none"
  }

  async count() {
    return this.terminals.size
  }

  async close() {
    for (const sid of [...this.terminals.keys()]) this.kill(sid)
  }
}
