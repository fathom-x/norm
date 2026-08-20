import type { CliRenderer } from "@opentui/core"
import { writeSync } from "node:fs"

// Everything the TUI turns on in the terminal, turned back off. destroy()
// hands the restore to the render thread, and the process can reach the tui
// command's process.exit(0) while a frame is still in flight — observed as
// the byte stream cutting off mid-frame with no restore ever written, leaving
// the shell inside the alternate screen with mouse reporting on. Writing the
// restore synchronously on fd 1 here cannot be truncated by a later exit.
// Every sequence is idempotent, so doubling up with destroy()'s own restore
// is harmless (popping an empty kitty-keyboard stack included).
const TERMINAL_RESTORE =
  "\x1b[?2026l" + // end synchronized update
  "\x1b[<u" + // pop kitty keyboard protocol
  "\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[?1006l" + // mouse reporting off
  "\x1b[?2004l" + // bracketed paste off
  "\x1b[?1049l" + // leave alternate screen
  "\x1b[?2031l" + // theme-change notifications off
  "\x1b[0 q" + // cursor style reset
  "\x1b[0m" + // SGR reset
  "\x1b[?25h" // show cursor

export function destroyRenderer(renderer: Pick<CliRenderer, "isDestroyed" | "setTerminalTitle" | "destroy">) {
  renderer.setTerminalTitle("")
  if (renderer.isDestroyed) return
  renderer.destroy()
  try {
    writeSync(1, TERMINAL_RESTORE)
  } catch {}
  // The process is expected to reach the tui command's own `process.exit(0)`
  // moments after the renderer goes down, but leaked handles can keep the
  // event loop alive with the terminal already restored — the shell shows no
  // prompt and raw-echoes keystrokes over the old frame until the process is
  // killed. The unref'd timer never delays a clean shutdown; it only ends a
  // wedged one.
  const watchdog = setTimeout(() => process.exit(0), 3000)
  watchdog.unref?.()
}
