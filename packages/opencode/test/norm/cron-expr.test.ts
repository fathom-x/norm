import { describe, expect, test } from "bun:test"
import { CronExpr } from "@/norm/cron-expr"

// Local time throughout, as the expressions are.
const at = (year: number, month: number, day: number, hour = 0, minute = 0, second = 0) =>
  new Date(year, month - 1, day, hour, minute, second)
const next = (source: string, after: Date) => CronExpr.next(CronExpr.parse(source), after)

describe("CronExpr", () => {
  test("every N minutes", () => {
    expect(next("*/5 * * * *", at(2026, 10, 3, 14, 2))).toEqual(at(2026, 10, 3, 14, 5))
    expect(next("*/5 * * * *", at(2026, 10, 3, 14, 55))).toEqual(at(2026, 10, 3, 15, 0))
    expect(next("* * * * *", at(2026, 10, 3, 14, 2, 30))).toEqual(at(2026, 10, 3, 14, 3))
  })

  test("is strictly after the given time", () => {
    expect(next("5 14 * * *", at(2026, 10, 3, 14, 5))).toEqual(at(2026, 10, 4, 14, 5))
    expect(next("5 14 * * *", at(2026, 10, 3, 14, 4, 59))).toEqual(at(2026, 10, 3, 14, 5))
  })

  test("daily and hourly", () => {
    expect(next("57 8 * * *", at(2026, 10, 3, 9))).toEqual(at(2026, 10, 4, 8, 57))
    expect(next("7 * * * *", at(2026, 12, 31, 23, 30))).toEqual(at(2027, 1, 1, 0, 7))
  })

  test("weekdays, with 7 as Sunday", () => {
    // 3 Oct 2026 is a Saturday.
    expect(next("0 9 * * 1-5", at(2026, 10, 3, 12))).toEqual(at(2026, 10, 5, 9))
    expect(next("0 9 * * 7", at(2026, 10, 3, 12))).toEqual(at(2026, 10, 4, 9))
    expect(next("0 9 * * 0", at(2026, 10, 3, 12))).toEqual(at(2026, 10, 4, 9))
  })

  test("a pinned date fires once a year", () => {
    expect(next("30 14 28 2 *", at(2026, 10, 3))).toEqual(at(2027, 2, 28, 14, 30))
    expect(next("0 0 29 2 *", at(2026, 10, 3))).toEqual(at(2028, 2, 29))
  })

  test("with both day fields restricted, either matches", () => {
    // The 15th, or any Monday: 5 Oct 2026 is a Monday.
    expect(next("0 0 15 * 1", at(2026, 10, 3))).toEqual(at(2026, 10, 5))
    expect(next("0 0 15 * 1", at(2026, 10, 12, 1))).toEqual(at(2026, 10, 15))
  })

  test("lists, ranges and steps", () => {
    const expr = CronExpr.parse("0,30 9-17/4 * 1,6 *")
    expect([...expr.minutes]).toEqual([0, 30])
    expect([...expr.hours]).toEqual([9, 13, 17])
    expect([...expr.months]).toEqual([1, 6])
    expect([...CronExpr.parse("5/20 * * * *").minutes]).toEqual([5, 25, 45])
  })

  test("an expression that never matches has no next time", () => {
    expect(next("0 0 31 2 *", at(2026, 10, 3))).toBeUndefined()
  })

  test("rejects malformed expressions with the reason", () => {
    expect(() => CronExpr.parse("* * * *")).toThrow("expected 5 fields")
    expect(() => CronExpr.parse("60 * * * *")).toThrow('bad minute field "60" (allowed 0-59)')
    expect(() => CronExpr.parse("* 24 * * *")).toThrow("bad hour field")
    expect(() => CronExpr.parse("* * 0 * *")).toThrow("bad day-of-month field")
    expect(() => CronExpr.parse("*/0 * * * *")).toThrow("bad minute field")
    expect(() => CronExpr.parse("5-1 * * * *")).toThrow("bad minute field")
    expect(() => CronExpr.parse("@daily")).toThrow("expected 5 fields")
    expect(() => CronExpr.parse("a * * * *")).toThrow("bad minute field")
  })
})
