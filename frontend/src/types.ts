// Data shapes shared by the dashboard (App) and the transactions table.

/** Which chart a pinned selection chip came from (mirrors the API enum). */
export type ChartKind = "yearly" | "cumulative" | "monthly" | "daily" | "categories" | "sankey"

/**
 * A pinned chart selection (bar, slice or point) sent with every chat
 * message. `value` is the raw CHF number from the API, before any
 * client-side currency conversion.
 */
export interface Selection {
  chart: ChartKind
  series: string
  label: string
  value: number
  year?: number
  month?: number
  category?: string
}

export function selectionKey(selection: Selection) {
  return [
    selection.chart,
    selection.series,
    selection.label,
    selection.year ?? "",
    selection.month ?? "",
    selection.category ?? "",
  ].join(":")
}

export interface Category {
  id: number
  name: string
  color: string
}

export interface Transaction {
  id: number
  dt: string
  description: string
  subject: string | null
  source: string
  account: string
  amount_chf: number
  currency_orig: string
  amount_orig: number
  kind: string
  is_transfer: boolean
  category: Category | null
}

export interface TransactionsResponse {
  items: Transaction[]
  total: number
  page: number
  page_size: number
  pages: number
}
