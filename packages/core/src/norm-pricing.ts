export * as NormPricing from "./norm-pricing"

// What overpay models cost, as owallet's `GET /v1/models` reports it (each
// entry's optional `pricing`, `context_length`, `active`; older owallets
// send ids only). Shared by the server-side norm layer (which turns it into
// opencode's model `cost`/`limit`) and the TUI (picker prices, next-message
// estimates, the sidebar).
//
// Prices are USD per million tokens with the seller's markup already in.
// They're *list* prices — the priciest provider the seller admitted at its
// last catalog fetch — so estimates from them run high; the charge is the
// turn's real cost plus markup, with a per-turn minimum (`min_charge`).

export type Tier = { min_input_tokens: number; input: number; output: number }

export type Pricing = {
  input: number
  output: number
  cache_read?: number
  /** Flat USD per request, where the model has one. */
  request?: number
  long_context?: Tier[]
  /** The least one turn is charged, in USD. */
  min_charge: number
  /** The model's minimum commitment: the least a turn may be authorized for. */
  min_authorization?: number
  as_of?: string
}

export type Model = {
  id: string
  name?: string
  contextLength?: number
  active?: boolean
  pricing?: Pricing
}

/** A reply's assumed length when nothing better is known. */
export const DEFAULT_OUTPUT_TOKENS = 1_500
/** opencode's own output cap (ProviderTransform.OUTPUT_TOKEN_MAX). */
export const OUTPUT_TOKEN_MAX = 32_000
// Mirrors owallet's `OPENROUTER_MIN_OUTPUT_TOKENS`: the reply the seller
// reserves before it runs a turn at all.
const MIN_OUTPUT_TOKENS = 256
// owallet sizes a turn's input from its JSON bytes at 3 bytes per token
// (`OPENROUTER_INPUT_BYTES_PER_TOKEN`); real tokenizers average nearer 4,
// so a usage-reported token count reads about 4/3 short of owallet's.
const OWALLET_INPUT_INFLATION = 4 / 3

const finite = (value: unknown): value is number => typeof value === "number" && Number.isFinite(value)

function parsePricing(raw: any): Pricing | undefined {
  if (!raw || !finite(raw.input) || !finite(raw.output)) return undefined
  const tiers = Array.isArray(raw.long_context)
    ? raw.long_context.filter(
        (tier: any): tier is Tier => finite(tier?.min_input_tokens) && finite(tier?.input) && finite(tier?.output),
      )
    : []
  return {
    input: raw.input,
    output: raw.output,
    cache_read: finite(raw.cache_read) ? raw.cache_read : undefined,
    request: finite(raw.request) ? raw.request : undefined,
    long_context: tiers.length ? tiers : undefined,
    min_charge: finite(raw.min_charge) ? raw.min_charge : 0.01,
    min_authorization: finite(raw.min_authorization) ? raw.min_authorization : undefined,
    as_of: typeof raw.as_of === "string" ? raw.as_of : undefined,
  }
}

/** A `GET /v1/models` body as models; undefined when it lists none. */
export function parseModels(body: unknown): Model[] | undefined {
  const data = (body as any)?.data
  const models = (Array.isArray(data) ? data : [])
    .filter((entry: any) => typeof entry?.id === "string" && entry.id.length > 0)
    .map(
      (entry: any): Model => ({
        id: entry.id,
        name: typeof entry.name === "string" && entry.name ? entry.name : undefined,
        contextLength: finite(entry.context_length) && entry.context_length > 0 ? entry.context_length : undefined,
        active: typeof entry.active === "boolean" ? entry.active : undefined,
        pricing: parsePricing(entry.pricing),
      }),
    )
  return models.length ? models : undefined
}

/** The rates in force for a prompt of `inputTokens` (the highest tier reached). */
export function rates(pricing: Pricing, inputTokens: number): { input: number; output: number } {
  let current = { input: pricing.input, output: pricing.output }
  for (const tier of pricing.long_context ?? []) {
    if (inputTokens >= tier.min_input_tokens) current = { input: tier.input, output: tier.output }
  }
  return current
}

const toCents = (usd: number) => Math.ceil(usd * 100 - 1e-9) / 100

/**
 * The expected charge for one step (one model call) in USD: a prompt of
 * `contextTokens`, `cachedTokens` of it read from the provider's prompt
 * cache, and a reply of `outputTokens` — rounded up to the cent and never
 * below the per-turn minimum, as the seller charges.
 */
export function estimateStep(
  pricing: Pricing,
  input: { contextTokens: number; cachedTokens?: number; outputTokens?: number },
): number {
  const context = Math.max(0, input.contextTokens)
  const cached = Math.min(context, Math.max(0, input.cachedTokens ?? 0))
  const output = Math.max(0, input.outputTokens ?? DEFAULT_OUTPUT_TOKENS)
  const rate = rates(pricing, context)
  const usd =
    ((context - cached) * rate.input + cached * (pricing.cache_read ?? rate.input) + output * rate.output) / 1e6 +
    (pricing.request ?? 0)
  return Math.max(pricing.min_charge, toCents(usd))
}

/**
 * The least owallet will authorize a step for — the prompt at full price
 * plus the seller's minimal reply, or the model's minimum commitment — so a
 * per-message limit below it means owallet refuses the message (mirrors
 * `size_openrouter_authorization`, including its byte-based token count).
 */
export function leastAuthorization(pricing: Pricing, contextTokens: number): number {
  const tokens = Math.ceil(Math.max(0, contextTokens) * OWALLET_INPUT_INFLATION)
  const rate = rates(pricing, tokens)
  const usd = (tokens * rate.input + MIN_OUTPUT_TOKENS * rate.output) / 1e6 + (pricing.request ?? 0)
  return Math.max(toCents(usd), pricing.min_authorization ?? 0, 0.01)
}

/** An assistant message's model and token usage, as opencode stores it. */
export type Turn = {
  modelID: string
  tokens: { input: number; output: number; cache: { read: number; write: number } }
}

/**
 * What the next step of a conversation would cost on `modelID`, from its
 * assistant turns so far (oldest first): the last prompt plus its reply is
 * the next prompt's context. On the same model, what the last turn read
 * from or wrote to the prompt cache is billed at the cache rate; on another
 * model nothing is cached yet (`switching`), so the whole context is fresh
 * input once. `least` is what owallet would need to authorize — the number
 * to hold against the per-message limit. Undefined before the first reply.
 */
export function nextStep(
  pricing: Pricing,
  modelID: string,
  turns: Turn[],
): { usd: number; least: number; switching: boolean } | undefined {
  const replied = turns.filter((turn) => turn.tokens.input + turn.tokens.output + turn.tokens.cache.read > 0)
  const last = replied.at(-1)
  if (!last) return undefined
  const t = last.tokens
  const contextTokens = t.input + t.cache.read + t.cache.write + t.output
  const recent = replied.slice(-5)
  const outputTokens = Math.round(recent.reduce((sum, turn) => sum + turn.tokens.output, 0) / recent.length)
  const switching = last.modelID !== modelID
  return {
    usd: estimateStep(pricing, {
      contextTokens,
      cachedTokens: switching ? 0 : t.cache.read + t.cache.write,
      outputTokens: outputTokens || DEFAULT_OUTPUT_TOKENS,
    }),
    least: leastAuthorization(pricing, contextTokens),
    switching,
  }
}

/** opencode's model `limit` for a context window: the output cap keeps
 * compaction's usable window (context − output) well above zero for small
 * windows, where opencode's 32k default would leave none. */
export function limit(contextLength: number): { context: number; output: number } {
  return { context: contextLength, output: Math.min(OUTPUT_TOKEN_MAX, Math.floor(contextLength / 4)) }
}

/** "$0.30/$2.40 per M" — input/output list price. */
export function perMillion(pricing: Pricing): string {
  return `${rate(pricing.input)}/${rate(pricing.output)} per M`
}

function rate(usd: number): string {
  if (usd === 0) return "$0"
  if (usd < 0.1) return `$${Number(usd.toPrecision(2))}`
  return `$${usd.toFixed(2)}`
}

/** "$0.04", for an estimate already rounded to the cent. */
export function money(usd: number): string {
  return `$${usd.toFixed(2)}`
}

// The models the TUI last fetched, so the picker and the sidebar share one
// read of `/v1/models` per process.
let known = new Map<string, Model>()
let loads = 0

export function remember(models: Model[]) {
  known = new Map(models.map((model) => [model.id, model]))
  loads++
}

/** Bumped on every `remember`, for UIs that poll for new prices. */
export function version(): number {
  return loads
}

export function get(id: string): Model | undefined {
  return known.get(id)
}
