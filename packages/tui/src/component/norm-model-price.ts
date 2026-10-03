import { createSignal } from "solid-js"
import { NormBudget } from "@opencode-ai/core/norm-budget"
import { NormPricing } from "@opencode-ai/core/norm-pricing"
import { useRoute } from "../context/route"
import { useSync } from "../context/sync"
import { useToast } from "../ui/toast"

// norm: what overpay models cost, for the model picker — the list price,
// or inside a conversation what its next message would cost on each model,
// flagging the ones owallet would refuse under the per-message limit. Kept
// out of dialog-model.tsx so the upstream file carries only the calls.

const PROVIDER_ID = "overpay"

type Cost = { input: number; output: number; cache?: { read: number } } | undefined

export function useNormModelPrice() {
  const route = useRoute()
  const sync = useSync()
  const toast = useToast()
  // In a conversation: what the next message would cost on each model, or
  // (toggled) the list price.
  const [listPrice, setListPrice] = createSignal(false)
  const [requestMax, setRequestMax] = createSignal<number | null>(NormBudget.DEFAULT_REQUEST_MAX_USD)
  void NormBudget.getRequestMax().then(setRequestMax, () => {})

  const turns = (): NormPricing.Turn[] => {
    if (route.data.type !== "session") return []
    return (sync.data.message[route.data.sessionID] ?? []).flatMap((message) =>
      message.role === "assistant" ? [{ modelID: message.modelID, tokens: message.tokens }] : [],
    )
  }

  // The full price (tiers, minimum charge) once the owallet plugin has read
  // /v1/models; until then the config's base rates.
  const pricing = (modelID: string, cost: Cost): NormPricing.Pricing | undefined => {
    const known = NormPricing.get(modelID)?.pricing
    if (known) return known
    if (!cost || (!cost.input && !cost.output)) return undefined
    return { input: cost.input, output: cost.output, cache_read: cost.cache?.read || undefined, min_charge: 0.01 }
  }

  const next = (providerID: string, modelID: string, cost: Cost) => {
    if (providerID !== PROVIDER_ID) return undefined
    const price = pricing(modelID, cost)
    if (!price) return undefined
    return { price, step: NormPricing.nextStep(price, modelID, turns()) }
  }

  return {
    /** Whether there's a "next message" price to toggle away from. */
    toggleable: () => route.data.type === "session",
    listPrice,
    togglePricing: () => setListPrice((value) => !value),
    footer(providerID: string, modelID: string, cost: Cost): string | undefined {
      const result = next(providerID, modelID, cost)
      if (!result) return undefined
      if (!result.step || listPrice()) return NormPricing.perMillion(result.price)
      const max = requestMax()
      if (max !== null && result.step.least > max) return `over your ${NormBudget.format(max)} limit`
      return `next ≈ ${NormPricing.money(result.step.usd)}`
    },
    /** After a switch mid-conversation: the new model reads the whole
     * conversation uncached once, which is where switching costs. */
    announceSwitch(providerID: string, modelID: string, cost: Cost) {
      const step = next(providerID, modelID, cost)?.step
      if (!step?.switching) return
      toast.show({
        message: `Switching re-reads this conversation on the new model once (≈ ${NormPricing.money(step.usd)}).`,
        variant: "info",
      })
    },
  }
}
