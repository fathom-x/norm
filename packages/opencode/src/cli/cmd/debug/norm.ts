import { EOL } from "os"
import { Effect } from "effect"
import { effectCmd } from "../../effect-cmd"

// norm: one JSON readout of the norm layer (owallet serve, provider key,
// Overpay link, marketplace models) — what the TUI sidebar shows, for scripts
// and coding agents that can't look at the TUI.
export const NormCommand = effectCmd({
  command: "norm",
  describe: "show norm's owallet/Overpay state as JSON",
  builder: (yargs) =>
    yargs.option("bootstrap", {
      describe: "run norm's startup bootstrap first (auto-start owallet serve, mint a provider key)",
      type: "boolean",
      default: false,
    }),
  instance: false,
  handler: Effect.fn("Cli.debug.norm")(function* (args) {
    const { Norm } = yield* Effect.promise(() => import("@/norm/norm"))
    if (args.bootstrap) yield* Effect.promise(() => Norm.bootstrap())
    const report = yield* Effect.promise(() => Norm.diagnose())
    process.stdout.write(JSON.stringify(report, null, 2) + EOL)
  }),
})
