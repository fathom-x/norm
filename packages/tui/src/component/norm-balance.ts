import { createSignal } from "solid-js"

// norm: the wallet's Overpay core-credit balance, in cents, as last read
// from owallet's `GET /v1/status` by the owallet sidebar plugin's poller
// (feature-plugins/sidebar/owallet.tsx). The prompt's hints row shows it
// beside the conversation's spend. Undefined until the first read, or when
// the wallet reports no core credits.
const [coreCents, setCoreCents] = createSignal<number | undefined>(undefined)

export const NormBalance = {
  coreCents,
  setCoreCents,
}
