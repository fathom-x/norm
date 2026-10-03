// Standard 5-field cron in local time: minute hour day-of-month month
// day-of-week. Each field takes `*`, numbers, ranges (`a-b`), lists (`a,b`)
// and steps (`*/n`, `a-b/n`). Day-of-week is 0-7, both 0 and 7 being Sunday.
// As in cron, when both day fields are restricted a day matches if either does.

export type CronExpr = {
  source: string
  minutes: Set<number>
  hours: Set<number>
  days: Set<number>
  months: Set<number>
  weekdays: Set<number>
  /** Whether the day-of-month / day-of-week field was `*`. */
  anyDay: boolean
  anyWeekday: boolean
}

const FIELDS = [
  { name: "minute", min: 0, max: 59 },
  { name: "hour", min: 0, max: 23 },
  { name: "day-of-month", min: 1, max: 31 },
  { name: "month", min: 1, max: 12 },
  { name: "day-of-week", min: 0, max: 7 },
] as const

// Far enough to reach the next Feb 29 from anywhere.
const HORIZON_DAYS = 366 * 8 + 2

export function parse(source: string): CronExpr {
  const parts = source.trim().split(/\s+/)
  if (parts.length !== 5)
    throw new Error(
      `Invalid cron expression "${source}": expected 5 fields (minute hour day-of-month month day-of-week), got ${parts.length}`,
    )
  const sets = parts.map((part, index) => field(part, FIELDS[index], source))
  return {
    source: parts.join(" "),
    minutes: sets[0],
    hours: sets[1],
    days: sets[2],
    months: sets[3],
    // 7 is Sunday too.
    weekdays: new Set(Array.from(sets[4], (day) => day % 7)),
    anyDay: parts[2].startsWith("*"),
    anyWeekday: parts[4].startsWith("*"),
  }
}

function field(text: string, spec: (typeof FIELDS)[number], source: string) {
  const fail = (): never => {
    throw new Error(
      `Invalid cron expression "${source}": bad ${spec.name} field "${text}" (allowed ${spec.min}-${spec.max})`,
    )
  }
  const number = (value: string) => {
    if (!/^\d+$/.test(value)) return fail()
    const parsed = Number(value)
    return parsed < spec.min || parsed > spec.max ? fail() : parsed
  }
  return new Set(
    text.split(",").flatMap((item) => {
      const [range, step, ...rest] = item.split("/")
      if (rest.length > 0 || range === "") return fail()
      const every = step === undefined ? 1 : number(step)
      if (every < 1) return fail()
      const [from, to, ...more] = range === "*" ? [String(spec.min), String(spec.max)] : range.split("-")
      if (more.length > 0) return fail()
      const start = number(from)
      // `5/15` means "from 5, every 15"; a bare `5` is just 5.
      const end = to === undefined ? (step === undefined ? start : spec.max) : number(to)
      if (end < start) return fail()
      return Array.from({ length: Math.floor((end - start) / every) + 1 }, (_, index) => start + index * every)
    }),
  )
}

function dayMatches(expr: CronExpr, date: Date) {
  if (!expr.months.has(date.getMonth() + 1)) return false
  const day = expr.days.has(date.getDate())
  const weekday = expr.weekdays.has(date.getDay())
  if (expr.anyDay && expr.anyWeekday) return true
  if (expr.anyDay) return weekday
  if (expr.anyWeekday) return day
  return day || weekday
}

/** The first matching minute strictly after `after`, or nothing if the
 * expression never matches (`0 0 31 2 *`). */
export function next(expr: CronExpr, after: Date): Date | undefined {
  const start = new Date(after.getTime())
  start.setSeconds(0, 0)
  start.setMinutes(start.getMinutes() + 1)
  const hours = Array.from(expr.hours).toSorted((a, b) => a - b)
  const minutes = Array.from(expr.minutes).toSorted((a, b) => a - b)

  for (let offset = 0; offset <= HORIZON_DAYS; offset++) {
    const day = new Date(start.getFullYear(), start.getMonth(), start.getDate() + offset)
    if (!dayMatches(expr, day)) continue
    for (const hour of hours) {
      for (const minute of minutes) {
        const candidate = new Date(day.getFullYear(), day.getMonth(), day.getDate(), hour, minute)
        // A skipped local time (the spring-forward hour) lands elsewhere; a
        // cron of that hour does not fire that day.
        if (candidate.getHours() !== hour || candidate.getMinutes() !== minute) continue
        if (candidate.getTime() >= start.getTime()) return candidate
      }
    }
  }
  return undefined
}

export * as CronExpr from "./cron-expr"
