import { describe, expect, test } from "bun:test"
import { usd } from "../../src/util/norm-money"

describe("usd", () => {
  test("keeps sub-cent spend visible below a dollar", () => {
    expect(usd(0.000756)).toBe("$0.0008")
    expect(usd(0.0123)).toBe("$0.0123")
    expect(usd(0.1508)).toBe("$0.1508")
  })

  test("whole cents read as before", () => {
    expect(usd(0)).toBe("$0.00")
    expect(usd(0.15)).toBe("$0.15")
    expect(usd(0.5)).toBe("$0.50")
  })

  test("two decimals from a dollar up", () => {
    expect(usd(1)).toBe("$1.00")
    expect(usd(1.23456)).toBe("$1.23")
    expect(usd(1234.5)).toBe("$1,234.50")
  })

  test("spend too small for four decimals is not shown as zero", () => {
    expect(usd(0.000001)).toBe("<$0.0001")
  })
})
