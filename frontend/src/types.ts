// Data shapes shared by the dashboard (App) and the transactions table.
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
