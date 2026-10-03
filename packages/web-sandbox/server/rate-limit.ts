// Per-key token buckets (per client IP for sandbox creation). In memory: a
// restart forgets them, which only ever errs on the side of letting a visitor
// in. Idle buckets are dropped once they are full again.

export class RateLimiter {
  private readonly buckets = new Map<string, { tokens: number; at: number }>()
  private readonly perMs: number

  constructor(
    /** Bucket size: how many in a burst. */
    readonly capacity: number,
    /** Refill: `capacity` tokens per this many ms. */
    readonly windowMs: number,
    private readonly now: () => number = Date.now,
  ) {
    this.perMs = capacity / windowMs
  }

  private refill(key: string) {
    const now = this.now()
    const bucket = this.buckets.get(key) ?? { tokens: this.capacity, at: now }
    bucket.tokens = Math.min(this.capacity, bucket.tokens + (now - bucket.at) * this.perMs)
    bucket.at = now
    return bucket
  }

  /** Take one token if there is one; false when the key is over its rate. */
  take(key: string): boolean {
    const bucket = this.refill(key)
    this.buckets.set(key, bucket)
    if (bucket.tokens < 1) return false
    bucket.tokens -= 1
    if (this.buckets.size > 10_000) this.prune()
    return true
  }

  /** Milliseconds until `key` has a token again (0 = now). */
  retryAfterMs(key: string): number {
    const bucket = this.refill(key)
    return bucket.tokens >= 1 ? 0 : Math.ceil((1 - bucket.tokens) / this.perMs)
  }

  prune() {
    for (const key of [...this.buckets.keys()]) if (this.refill(key).tokens >= this.capacity) this.buckets.delete(key)
  }
}
