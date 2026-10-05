import type { Hooks, PluginInput } from "@opencode-ai/plugin"
import { Norm } from "@/norm/norm"
import { OwalletRevive } from "@/norm/revive"
import { NormBudget } from "@opencode-ai/core/norm-budget"
import { NormAgentModels } from "@opencode-ai/core/norm-agent-models"

/**
 * norm's built-in plugin: runs the owallet bootstrap (auto-start the server,
 * mint a provider key) before providers load, registers the manual "paste
 * an API key" auth method for the `overpay` provider as the fallback when the
 * bootstrap can't provision one non-interactively, and merges the
 * marketplace's live model list into the provider config. Keys are minted in
 * the owallet dashboard (or `owallet provider-key create`).
 */
export async function NormOwalletPlugin(input: PluginInput): Promise<Hooks> {
  await Norm.bootstrap()
  const sessions = Norm.sessionAccess(input.client)
  // owallet was restarted under a running norm (norm/revive.ts): say so, and
  // reconnect its tools, whose MCP session died with the old process.
  OwalletRevive.onRevive(input.directory, () => {
    void input.client.tui
      .showToast({ body: { message: "owallet had stopped. norm restarted it.", variant: "info" } })
      .catch(() => {})
    void input.client.mcp.connect({ path: { name: Norm.MCP_NAME } }).catch(() => {})
  })
  return {
    // Per-conversation budget (/budget): send the conversation's remaining
    // allowance so owallet enforces it server-side for the whole request —
    // one turn can loop through several paid tool calls. At $0 owallet
    // refuses before placing any order. A conversation set to "no limit"
    // sends nothing; the key's daily budget still applies either way.
    "chat.headers": async (hook, output) => {
      if (Norm.disabled() || hook.model.providerID !== Norm.PROVIDER_ID) return
      // The conversation's key for OpenRouter's sticky routing: every turn of
      // one session lands on the same upstream provider, so its prompt cache
      // stays warm from the first turn. owallet never forwards it as-is — it
      // sends an HMAC of it, and only once Overpay accepts the field.
      output.headers["x-session-id"] = hook.sessionID
      // Titles, summaries and compaction send no tools, which owallet would
      // otherwise answer with its own server-side loop and whole tool
      // roster: a bigger prompt, and a model that could buy something
      // mid-title. Ask for a plain completion instead.
      if (Norm.PLAIN_AGENTS.has(hook.agent)) output.headers[Norm.TOOLS_HEADER] = "none"
      // Per-message limit (/budget → "Per-message limit"): the most one
      // message may authorize. owallet sizes each turn's hold within it and
      // refuses — before charging anything — a message that can't fit.
      const requestMax = await NormBudget.getRequestMax().catch(() => NormBudget.DEFAULT_REQUEST_MAX_USD)
      if (requestMax !== null) output.headers[NormBudget.REQUEST_MAX_HEADER] = requestMax.toFixed(2)
      const budget = await NormBudget.status(sessions, hook.sessionID).catch(() => undefined)
      if (!budget || budget.remaining === null) return
      output.headers[NormBudget.SPEND_LIMIT_HEADER] = budget.remaining.toFixed(2)
    },
    config: async (config) => {
      // Offer the marketplace's real model list (GET /v1/models), not just
      // the seeded `default` sentinel. Entries the user configured
      // themselves are left untouched; on any fetch failure the seeded
      // default remains the lone (and always valid) option.
      if (Norm.disabled()) return

      // Send the provider key on the owallet MCP connection too: owallet
      // accepts owk_ bearers on /mcp, binding the session to the key's
      // wallet and carrying its scopes + daily budget onto MCP purchases
      // (one credential, one budget, both surfaces). Only the seeded
      // owallet entry is touched, and user-configured headers win.
      const mcp = config.mcp?.[Norm.MCP_NAME]
      if (mcp && mcp.type === "remote" && !mcp.headers) {
        const key = await Norm.readProviderKey()
        // Version-gated: an older serve would 401 the bearer and sever
        // the MCP connection outright, where anonymous still works.
        if (key && (await Norm.mcpAcceptsProviderKeys())) {
          mcp.headers = { Authorization: `Bearer ${key}` }
        }
      }

      const overpay = config.provider?.[Norm.PROVIDER_ID]
      const models = overpay ? await Norm.marketplaceModels() : undefined
      if (overpay && models) overpay.models = Norm.mergeModels(overpay.models ?? {}, models)

      // /compaction-model, /title-model and the title prompt: after the
      // model list, so a choice the marketplace no longer offers is skipped
      // rather than failing, and the automatic title model can be priced.
      const [compaction, title] = await Promise.all(
        (["compaction", "title"] as const).map((agent) => NormAgentModels.get(agent).catch(() => undefined)),
      )
      Norm.applyAgentModels(config, { compaction, title }, models)
    },
    auth: {
      provider: Norm.PROVIDER_ID,
      methods: [
        {
          type: "api",
          label: "owallet provider key (owk_...)",
        },
      ],
    },
  }
}
