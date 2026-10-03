import { NormPricing } from "@opencode-ai/core/norm-pricing"

// norm: what the "Default" variant of an overpay model means — the
// seller's default effort, which it applies when a turn names none
// (the variant's `metadata.reasoning.default_effort`, read off owallet's
// `/v1/models` by the owallet plugin). Kept out of dialog-variant.tsx so
// the upstream file carries only the call.

const PROVIDER_ID = "overpay"

/** The footer for the "Default" option, or undefined off the overpay provider. */
export function defaultVariantFooter(providerID: string | undefined, modelID: string | undefined) {
  if (providerID !== PROVIDER_ID || !modelID) return undefined
  return NormPricing.defaultEffort(modelID)
}
