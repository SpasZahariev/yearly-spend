import { EmptyState } from "@/components/EmptyState"
import { TransactionsTable } from "@/components/TransactionsTable"
import type { Category } from "@/types"

export function TransactionsPage({
  year,
  categories,
  format,
}: {
  year: number | null
  categories: Category[]
  format: (value: number) => string
}) {
  if (year === null) {
    return <EmptyState />
  }
  return (
    <main className="min-w-0 flex-1 px-4 py-6 sm:px-6 sm:py-8 lg:overflow-y-auto">
      <div className="mx-auto flex w-full max-w-5xl flex-col gap-4 sm:gap-6">
        <TransactionsTable key={year} year={year} categories={categories} format={format} />
      </div>
    </main>
  )
}
