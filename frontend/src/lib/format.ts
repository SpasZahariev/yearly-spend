// Shared period/currency types and display formatters used by the app shell
// and both pages.

export type View = "month" | "year"
export type Granularity = "month" | "day"
export type Currency = "CHF" | "USD" | "EUR"

export interface Period {
  year: number
  month: number
}

export interface FxState {
  rate: number
  date: string
}

export const MONTH_LABELS = [
  "Jan",
  "Feb",
  "Mar",
  "Apr",
  "May",
  "Jun",
  "Jul",
  "Aug",
  "Sep",
  "Oct",
  "Nov",
  "Dec",
]

export const CURRENCIES: readonly Currency[] = ["CHF", "USD", "EUR"]

// currencyDisplay "code" keeps every currency rendered as `CHF 1'234.56`
// / `USD 1'234.56` / `EUR 1'234.56`, matching the pixel app's style.
export const moneyFormatters: Record<Currency, Intl.NumberFormat> = {
  CHF: new Intl.NumberFormat("de-CH", {
    style: "currency",
    currency: "CHF",
    currencyDisplay: "code",
  }),
  USD: new Intl.NumberFormat("de-CH", {
    style: "currency",
    currency: "USD",
    currencyDisplay: "code",
  }),
  EUR: new Intl.NumberFormat("de-CH", {
    style: "currency",
    currency: "EUR",
    currencyDisplay: "code",
  }),
}

export const compactNumber = new Intl.NumberFormat("de-CH", {
  notation: "compact",
  maximumFractionDigits: 1,
})

export function periodLabel(view: View, year: number, month: number) {
  return view === "month" ? `${MONTH_LABELS[month - 1]} ${year}` : String(year)
}

export function defaultMonthFor(year: number, periods: Period[]) {
  const months = periods.filter((period) => period.year === year).map((period) => period.month)
  return months.length > 0 ? Math.max(...months) : 12
}
