import { EOL } from "os"
import { Effect } from "effect"
import { NormBudget } from "@opencode-ai/core/norm-budget"
import { effectCmd, fail } from "../effect-cmd"
import { Session } from "@/session/session"
import { SessionID } from "../../session/schema"

// norm: the TUI's `/budget` dialog and sidebar figures, from the command line —
// so a script (or a coding agent testing norm through `norm run`) can read and
// set the same limits the TUI does. Storage and accounting are NormBudget's.
export const BudgetCommand = effectCmd({
  command: "budget [sessionID]",
  describe: "show or set norm's spending limits for a conversation (JSON)",
  builder: (yargs) =>
    yargs
      .positional("sessionID", {
        describe: "session id (defaults to the most recent conversation in this directory)",
        type: "string",
      })
      .option("set", {
        describe: 'conversation budget in USD, or "off" for no per-conversation limit',
        type: "string",
      })
      .option("request-max", {
        describe: 'per-message limit in USD for every conversation, or "off"',
        type: "string",
      }),
  // Resolving "the most recent conversation" lists the project's sessions.
  instance: (args) => !args.sessionID,
  handler: Effect.fn("Cli.budget")(function* (args) {
    const svc = yield* Session.Service

    const sessionID = args.sessionID
      ? SessionID.make(args.sessionID)
      : (yield* svc.list({ roots: true, limit: 1 }))[0]?.id
    if (!sessionID) return yield* fail("No sessions yet — pass a session id")
    yield* svc.get(sessionID).pipe(Effect.catch(() => fail(`Session not found: ${sessionID}`)))

    const access: NormBudget.SessionAccess = {
      parentOf: (id) =>
        Effect.runPromise(
          svc.get(SessionID.make(id)).pipe(
            Effect.map((s) => s.parentID || undefined),
            Effect.orElseSucceed(() => undefined),
          ),
        ),
      childrenOf: (id) =>
        Effect.runPromise(svc.children(SessionID.make(id)).pipe(Effect.map((list) => list.map((s) => s.id)))),
      costOf: (id) =>
        Effect.runPromise(
          svc.messages({ sessionID: SessionID.make(id) }).pipe(
            Effect.map((msgs) =>
              msgs.reduce(
                (sum, m) => sum + (m.info.role === "assistant" && Number.isFinite(m.info.cost) ? m.info.cost : 0),
                0,
              ),
            ),
            Effect.orElseSucceed(() => 0),
          ),
        ),
    }

    if (args.set !== undefined) {
      const usd = NormBudget.parse(args.set)
      if (usd === undefined) return yield* fail(`Invalid budget: ${args.set} (a USD amount like 2.50, or "off")`)
      const root = yield* Effect.promise(() => NormBudget.rootOf(access, sessionID))
      yield* Effect.promise(() => NormBudget.set(root, usd))
    }
    if (args["request-max"] !== undefined) {
      const usd = NormBudget.parse(args["request-max"])
      if (usd === undefined) return yield* fail(`Invalid per-message limit: ${args["request-max"]}`)
      yield* Effect.promise(() => NormBudget.setRequestMax(usd))
    }

    const status = yield* Effect.promise(() => NormBudget.status(access, sessionID))
    const requestMax = yield* Effect.promise(() => NormBudget.getRequestMax())
    process.stdout.write(
      JSON.stringify(
        {
          session: sessionID,
          conversation: status.root,
          budget_usd: status.budget,
          spent_usd: status.spent,
          remaining_usd: status.remaining,
          request_max_usd: requestMax,
        },
        null,
        2,
      ) + EOL,
    )
  }),
})
