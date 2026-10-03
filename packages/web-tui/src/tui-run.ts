// The TUI itself, loaded by tui.ts once the terminal, the process, the file
// system and the owallet route exist. Mirrors the worker branch of
// packages/opencode/src/cli/cmd/tui.ts.
import { Effect } from "effect"
import type { GlobalEvent } from "@opencode-ai/sdk/v2"
import { run } from "opencode/cli/tui/layer"
import { validateSession } from "opencode/cli/tui/validate-session"
import { TuiConfig } from "opencode/config/tui"
import { createLegacyTuiPluginHost } from "opencode/plugin/tui/runtime"
import { OPENCODE_ORIGIN } from "./fetch-router"
import { WORKSPACE } from "./env"
import type { TuiOptions } from "./tui"

export async function runTui(options: TuiOptions) {
  const { core } = options
  const config = await TuiConfig.get()
  await validateSession({ url: OPENCODE_ORIGIN, sessionID: options.sessionID, directory: WORKSPACE, fetch: core.fetch })
  await Effect.runPromise(
    run({
      url: OPENCODE_ORIGIN,
      config,
      pluginHost: createLegacyTuiPluginHost(),
      directory: WORKSPACE,
      fetch: core.fetch,
      events: {
        subscribe: async (handler: (event: GlobalEvent) => void) => core.onEvent(handler),
      },
      args: {
        sessionID: options.sessionID,
        prompt: options.prompt,
      },
    }),
  )
}
