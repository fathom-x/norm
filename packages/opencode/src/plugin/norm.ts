import type { Hooks, PluginInput } from "@opencode-ai/plugin"
import { Norm } from "@/norm/norm"

/**
 * norm's built-in plugin: runs the owallet bootstrap (auto-start the server,
 * mint a provider key) before providers load, and registers the manual "paste
 * an API key" auth method for the `overpay` provider as the fallback when the
 * bootstrap can't provision one non-interactively. Keys are minted in the
 * owallet dashboard (or `owallet provider-key create`).
 */
export async function NormOwalletPlugin(_input: PluginInput): Promise<Hooks> {
  await Norm.bootstrap()
  return {
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
