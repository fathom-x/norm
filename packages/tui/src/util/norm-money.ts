const cents = new Intl.NumberFormat("en-US", { style: "currency", currency: "USD" })
const micro = new Intl.NumberFormat("en-US", {
  style: "currency",
  currency: "USD",
  minimumFractionDigits: 2,
  maximumFractionDigits: 4,
})

/**
 * Dollars for display. Overpay charges exactly, down to $0.000001, so a
 * turn often costs less than a cent: below $1 show up to four decimals
 * ($0.0008, $0.15) rather than rounding real spend to $0.00. From $1 up the
 * sub-cent part is noise and two decimals are enough.
 */
export function usd(value: number) {
  if (value > 0 && value < 0.00005) return "<$0.0001"
  if (Math.abs(value) < 1) return micro.format(value)
  return cents.format(value)
}
