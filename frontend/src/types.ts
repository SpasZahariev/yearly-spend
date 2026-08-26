// Data shapes shared by the dashboard (App) and the transactions table.

/** Which chart a pinned selection chip came from (mirrors the API enum). */
export type ChartKind =
  "yearly" | "cumulative" | "monthly" | "daily" | "categories" | "sankey" | "summary"

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
  note?: string
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

/**
 * Dashboard chart data pushed by the chat's `render_dashboard` tool
 * (streamed as `event: chart`). All amounts are positive CHF magnitudes,
 * before client-side currency conversion.
 */
export interface ChartUpdate {
  year: number
  /** Short label of the period the data covers, e.g. "2024" or "2024-03". */
  label: string
  kpi?: { income: number; spend: number; moved: number }
  monthly?: { month: number; value: number }[]
  yearly?: { year: number; value: number }[]
  cumulative?: { month: number; value: number }[]
  categories?: { name: string; value: number }[]
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
