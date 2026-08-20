import type { CliRenderer } from "@opentui/core"

export function destroyRenderer(renderer: Pick<CliRenderer, "isDestroyed" | "setTerminalTitle" | "destroy">) {
  renderer.setTerminalTitle("")
  if (renderer.isDestroyed) return
  renderer.destroy()
  // The process is expected to reach the tui command's own `process.exit(0)`
  // moments after the renderer goes down, but leaked handles can keep the
  // event loop alive with the terminal already restored — the shell shows no
  // prompt and raw-echoes keystrokes over the old frame until the process is
  // killed. The unref'd timer never delays a clean shutdown; it only ends a
  // wedged one.
  const watchdog = setTimeout(() => process.exit(0), 3000)
  watchdog.unref?.()
}
