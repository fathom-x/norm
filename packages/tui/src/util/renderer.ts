import type { CliRenderer } from "@opentui/core"
import { writeSync } from "node:fs"

// The input modes the TUI turns on: while any is still on after the tty is
// back in cooked mode, mouse movement and modified keys reach the shell as
// raw escape sequences ("crazy characters" on mouse-over). opentui enables
// modifyOtherKeys (CSI >4;1m) and restores neither it nor anything else
// until its native teardown, which runs after it has already left raw mode.
const INPUT_RESTORE =
  "\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[?1006l" + // mouse reporting off
  "\x1b[?2004l" + // bracketed paste off
  "\x1b[?1004l" + // focus reporting off
  "\x1b[<u" + // pop kitty keyboard protocol
  "\x1b[>4m" // modifyOtherKeys back to the terminal's default

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
  INPUT_RESTORE +
  "\x1b[?1049l" + // leave alternate screen
  "\x1b[?2031l" + // theme-change notifications off
  "\x1b[0 q" + // cursor style reset
  "\x1b[0m" + // SGR reset
  "\x1b[?25h" // show cursor

function write(sequence: string) {
  try {
    writeSync(1, sequence)
  } catch {}
}

// Last bytes out, after the epilogue: input modes only, in case anything
// turned one back on after destroy. A repeated alternate-screen exit restores
// the cursor saved at startup on some terminals (Terminal.app), which would
// jump back over the epilogue.
export function restoreTerminalInput() {
  write(INPUT_RESTORE + "\x1b[?25h")
}

let exiting = false
function onSigint() {
  // Once the renderer has left raw mode, ctrl+c is a real SIGINT again; a
  // second press used to kill the shutdown halfway. Let it finish — the
  // watchdog below still bounds it.
}

export function destroyRenderer(renderer: Pick<CliRenderer, "isDestroyed" | "setTerminalTitle" | "destroy">) {
  renderer.setTerminalTitle("")
  if (renderer.isDestroyed) return
  // Before destroy(): it drops raw mode first, and any mouse report the
  // terminal sends in between is echoed or handed to the shell as text.
  write(INPUT_RESTORE)
  if (!exiting) {
    exiting = true
    process.on("SIGINT", onSigint)
  }
  renderer.destroy()
  write(TERMINAL_RESTORE)
  // The process is expected to reach the tui command's own `process.exit(0)`
  // moments after the renderer goes down, but leaked handles can keep the
  // event loop alive with the terminal already restored — the shell shows no
  // prompt and raw-echoes keystrokes over the old frame until the process is
  // killed. The unref'd timer never delays a clean shutdown; it only ends a
  // wedged one.
  const watchdog = setTimeout(() => process.exit(0), 3000)
  watchdog.unref?.()
}
