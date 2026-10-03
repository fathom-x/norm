// Browser twin of shell.ts, swapped in by the web build (packages/web-tui).
// A browser tab has no shell and no processes, so `bash` stays registered —
// prompts, agents and permissions keep naming it — but tells the model why it
// cannot run anything and where code execution lives instead. It also skips
// shell.ts's tree-sitter grammars (bash/PowerShell wasm), which exist only to
// pre-scan commands for permissions.
import { Effect, Schema } from "effect"
import * as Tool from "./tool"
import { ShellID } from "./shell/id"
import { Parameters } from "./shell/prompt"

export { Parameters } from "./shell/prompt"

export const UNAVAILABLE =
  "The bash tool is not available in the browser build: a browser tab has no shell or processes. " +
  "Use the file tools (read, write, edit, glob, grep) for the workspace, and the marketplace run_python tool to run code."

export const ShellTool = Tool.define(
  ShellID.ToolID,
  Effect.succeed({
    description: UNAVAILABLE,
    parameters: Parameters,
    execute: (params: Schema.Schema.Type<typeof Parameters>) =>
      Effect.succeed({
        title: params.command,
        metadata: { output: UNAVAILABLE, exit: 127, truncated: false },
        output: UNAVAILABLE,
      }),
  }),
)
