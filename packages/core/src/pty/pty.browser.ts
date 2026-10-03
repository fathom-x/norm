// Browser variant of #pty: there are no pseudo-terminals in a tab.
import type { Opts, Proc } from "./pty"

export type { Disp, Exit, Opts, Proc } from "./pty"

export function spawn(_file: string, _args: string[], _opts: Opts): Proc {
  throw new Error("Terminals are not available in the browser build")
}
