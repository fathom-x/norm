// What the broker needs from wherever the terminal actually runs: a private
// sandbox per visitor (`sid`), with one long-lived terminal process in it.
//
// Two implementations: `providers/e2b.ts` (production — an E2B sandbox per
// visitor, paused between visits) and `providers/local.ts` (a PTY on this
// machine — development and the browser tests).

/** What a visitor's sandbox is doing, as far as the provider knows. */
export type SandboxState = "none" | "running" | "paused"

export interface TerminalSize {
  cols: number
  rows: number
}

/** Progress the broker relays to the page while a terminal comes up. */
export type ProviderPhase = "creating" | "resuming" | "starting"

export interface FindOrCreateOptions extends TerminalSize {
  /**
   * Called right before a NEW sandbox would be created (never for an existing
   * one). Throws `LimitError` to refuse (sandbox cap, per-IP rate).
   */
  admit(): void | Promise<void>
  /** Progress for the page's status overlay. */
  onPhase?(phase: ProviderPhase): void
}

export interface AttachOptions extends TerminalSize {
  /** Terminal output (raw bytes). */
  onData(bytes: Uint8Array): void
  /** The terminal process ended (the next attach starts a new one). */
  onExit(code: number | undefined): void
}

/** A live connection to the terminal process. */
export interface Attachment {
  write(bytes: Uint8Array): void
  resize(cols: number, rows: number): void
  /** Stop receiving output; the process keeps running. */
  detach(): void
}

export interface SandboxHandle {
  /**
   * Connect to the sandbox's terminal process: reattach if it is alive (the
   * provider makes the program redraw for the new size), else start it.
   */
  attach(options: AttachOptions): Promise<Attachment>
}

export interface SandboxProvider {
  readonly name: string
  /** The visitor's sandbox, resumed if paused, created if missing. */
  findOrCreate(sid: string, options: FindOrCreateOptions): Promise<SandboxHandle>
  /** Delete the visitor's sandbox and everything in it. */
  reset(sid: string): Promise<void>
  /** Nobody is watching: let the sandbox sleep (state kept). */
  pause(sid: string): Promise<void>
  /** What the visitor's sandbox is doing. */
  status(sid: string): Promise<SandboxState>
  /** Sandboxes currently running (what `MAX_SANDBOXES` caps). */
  count(): Promise<number>
  /**
   * Delete this app's paused sandboxes created more than `maxAgeMs` ago (the
   * retention sweeper); returns how many went. Optional: the local provider
   * keeps nothing worth sweeping beyond the process's life.
   */
  sweep?(maxAgeMs: number): Promise<number>
  /** Stop everything this process started (server shutdown). */
  close?(): Promise<void>
}

/** A refusal the page shows as-is (capacity, rate limit). */
export class LimitError extends Error {
  override readonly name = "LimitError"
}
