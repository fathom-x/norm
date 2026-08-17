import type { Hooks, PluginInput } from "@opencode-ai/plugin"
import { Norm } from "@/norm/norm"

/**
 * norm's built-in plugin: runs the owallet bootstrap (auto-start the server,
 * mint a provider key) before providers load, registers the manual "paste
 * an API key" auth method for the `overpay` provider as the fallback when the
 * bootstrap can't provision one non-interactively, and merges the
 * marketplace's live model list into the provider config. Keys are minted in
 * the owallet dashboard (or `owallet provider-key create`).
 */
export async function NormOwalletPlugin(_input: PluginInput): Promise<Hooks> {
  await Norm.bootstrap()
  return {
    config: async (config) => {
      // Offer the marketplace's real model list (GET /v1/models), not just
      // the seeded `default` sentinel. Entries the user configured
      // themselves are left untouched; on any fetch failure the seeded
      // default remains the lone (and always valid) option.
      if (Norm.disabled()) return
      const overpay = config.provider?.[Norm.PROVIDER_ID]
      if (!overpay) return
      const ids = await Norm.marketplaceModels()
      if (!ids) return
      overpay.models ??= {}
      for (const id of ids) {
        if (!overpay.models[id]) overpay.models[id] = { name: id === "default" ? "Overpay marketplace (default)" : id }
      }
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
