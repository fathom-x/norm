import { expect, test } from "bun:test"
import { RateLimiter } from "../../server/rate-limit"

test("a bucket allows its capacity, then refills over the window", () => {
  let now = 0
  const limiter = new RateLimiter(5, 3_600_000, () => now)
  for (let i = 0; i < 5; i++) expect(limiter.take("1.2.3.4")).toBe(true)
  expect(limiter.take("1.2.3.4")).toBe(false)
  // Another key has its own bucket.
  expect(limiter.take("5.6.7.8")).toBe(true)
  // One token takes window/capacity = 12 minutes.
  expect(limiter.retryAfterMs("1.2.3.4")).toBe(720_000)
  now += 719_000
  expect(limiter.take("1.2.3.4")).toBe(false)
  now += 1_000
  expect(limiter.take("1.2.3.4")).toBe(true)
  expect(limiter.take("1.2.3.4")).toBe(false)
  // Never more than the capacity, however long it waited.
  now += 100 * 3_600_000
  for (let i = 0; i < 5; i++) expect(limiter.take("1.2.3.4")).toBe(true)
  expect(limiter.take("1.2.3.4")).toBe(false)
})

test("prune drops full buckets only", () => {
  let now = 0
  const limiter = new RateLimiter(2, 1000, () => now)
  limiter.take("a")
  limiter.take("b")
  limiter.take("b")
  now += 500
  limiter.prune()
  // "a" refilled (1 + 1 token) and was dropped; "b" (1 token) kept its state.
  expect(limiter.take("b")).toBe(true)
  expect(limiter.take("b")).toBe(false)
})
