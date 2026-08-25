import { useEffect, useState } from "react"
import { EditorialTitle } from "@/components/EditorialTitle"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Card, CardContent, CardDescription, CardHeader } from "@/components/ui/card"
import { getJson, patchJson } from "@/lib/api"
import { cn } from "@/lib/utils"
import type { Category, Transaction, TransactionsResponse } from "@/types"

const PAGE_SIZE = 50

const MONTH_NAMES = [
  "jan",
  "feb",
  "mar",
  "apr",
  "may",
  "jun",
  "jul",
  "aug",
  "sep",
  "oct",
  "nov",
  "dec",
]

const SOURCE_OPTIONS = [
  { value: "all", label: "all sources" },
  { value: "neon", label: "neon" },
  { value: "revolut", label: "revolut" },
  { value: "cashback", label: "cashback" },
]

const MONTH_OPTIONS = [
  { value: "all", label: "all months" },
  ...MONTH_NAMES.map((name, index) => ({ value: String(index + 1), label: name })),
]

function amountClass(kind: string) {
  if (kind === "income") return "text-green-600"
  if (kind === "spend") return "text-brand-pink"
  return "text-muted-foreground"
}

interface TransactionsTableProps {
  year: number
  categories: Category[]
  format: (value: number) => string
}

export function TransactionsTable({ year, categories, format }: TransactionsTableProps) {
  const [source, setSource] = useState("all")
  const [category, setCategory] = useState("all")
  const [month, setMonth] = useState("all")
  const [page, setPage] = useState(1)
  const [data, setData] = useState<TransactionsResponse | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [saving, setSaving] = useState<ReadonlySet<number>>(new Set())

  // Reset pagination whenever a scope filter changes, done in the handlers
  // below (the component remounts via `key` when the year changes).
  useEffect(() => {
    let cancelled = false
    const params = new URLSearchParams()
    params.set("year", String(year))
    params.set("page", String(page))
    params.set("page_size", String(PAGE_SIZE))
    if (source !== "all") params.set("source", source)
    if (category !== "all") params.set("category", category)
    if (month !== "all") params.set("month", month)
    getJson<TransactionsResponse>(`/api/transactions?${params.toString()}`)
      .then((res) => {
        if (cancelled) return
        setData(res)
        setError(null)
      })
      .catch((err: unknown) => {
        if (!cancelled) setError(err instanceof Error ? err.message : String(err))
      })
    return () => {
      cancelled = true
    }
  }, [year, source, category, month, page])

  function override(tx: Transaction, body: { category?: string; is_transfer?: boolean }) {
    setSaving((prev) => new Set(prev).add(tx.id))
    return patchJson<Transaction>(`/api/transactions/${tx.id}`, body)
      .then((updated) => {
        setData((prev) =>
          prev
            ? { ...prev, items: prev.items.map((it) => (it.id === updated.id ? updated : it)) }
            : prev,
        )
      })
      .catch((err: unknown) => setError(err instanceof Error ? err.message : String(err)))
      .finally(() =>
        setSaving((prev) => {
          const next = new Set(prev)
          next.delete(tx.id)
          return next
        }),
      )
  }

  const items = data?.items ?? []
  const total = data?.total ?? 0
  const pages = data?.pages ?? 0

  return (
    <Card className="animate-step-in">
      <CardHeader className="flex-row items-start justify-between gap-4 border-b border-border">
        <div className="flex flex-col gap-1.5">
          <EditorialTitle title="transactions" tag={String(year)} />
          <CardDescription>
            inline overrides · {year} · {total} rows
          </CardDescription>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <FilterSelect
            name="filter-source"
            value={source}
            onChange={(next) => {
              setSource(next)
              setPage(1)
            }}
            options={SOURCE_OPTIONS}
          />
          <FilterSelect
            name="filter-category"
            value={category}
            onChange={(next) => {
              setCategory(next)
              setPage(1)
            }}
            options={categoryOptions(categories)}
          />
          <FilterSelect
            name="filter-month"
            value={month}
            onChange={(next) => {
              setMonth(next)
              setPage(1)
            }}
            options={MONTH_OPTIONS}
          />
        </div>
      </CardHeader>
      <CardContent>
        {error !== null ? (
          <div className="flex h-40 items-center justify-center font-mono text-sm text-destructive">
            error: {error}
          </div>
        ) : items.length === 0 ? (
          <div className="flex h-40 items-center justify-center font-mono text-sm text-muted-foreground">
            no transactions match
          </div>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full border-collapse text-sm">
              <thead>
                <tr className="border-b border-border text-left text-[11px] font-medium uppercase tracking-[0.12em] text-muted-foreground">
                  <th className="py-2 pr-3 font-medium">date</th>
                  <th className="py-2 pr-3 font-medium">description</th>
                  <th className="py-2 pr-3 font-medium">source</th>
                  <th className="py-2 pr-3 text-right font-medium">amount</th>
                  <th className="py-2 pr-3 font-medium">category</th>
                  <th className="py-2 text-right font-medium">transfer</th>
                </tr>
              </thead>
              <tbody>
                {items.map((tx) => {
                  const busy = saving.has(tx.id)
                  return (
                    <tr key={tx.id} className="border-b border-border align-top hover:bg-accent/60">
                      <td className="whitespace-nowrap py-2 pr-3 font-mono text-xs text-foreground">
                        {tx.dt}
                      </td>
                      <td className="max-w-72 py-2 pr-3">
                        <div className="truncate" title={tx.description}>
                          {tx.description}
                        </div>
                        {tx.subject !== null && (
                          <div
                            className="truncate font-mono text-[11px] text-muted-foreground"
                            title={tx.subject}
                          >
                            {tx.subject}
                          </div>
                        )}
                      </td>
                      <td className="whitespace-nowrap py-2 pr-3">
                        <Badge variant="outline">{tx.source}</Badge>
                      </td>
                      <td
                        className={cn(
                          "whitespace-nowrap py-2 pr-3 text-right font-mono",
                          amountClass(tx.kind),
                        )}
                      >
                        {tx.amount_chf >= 0 ? "+" : ""}
                        {format(tx.amount_chf)}
                      </td>
                      <td className="whitespace-nowrap py-2 pr-3">
                        <div className="flex items-center gap-2">
                          <span
                            aria-hidden="true"
                            className="size-3 shrink-0 border border-border"
                            style={{ backgroundColor: tx.category?.color ?? "var(--muted)" }}
                          />
                          <select
                            name={`tx-category-${tx.id}`}
                            value={tx.category?.name ?? "uncategorized"}
                            disabled={busy}
                            onChange={(event) => override(tx, { category: event.target.value })}
                            className="h-8 rounded-none border border-input bg-background px-1.5 text-xs text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-50"
                          >
                            {categories.map((option) => (
                              <option key={option.name} value={option.name}>
                                {option.name}
                              </option>
                            ))}
                          </select>
                        </div>
                      </td>
                      <td className="py-2 text-right">
                        <input
                          type="checkbox"
                          name={`tx-transfer-${tx.id}`}
                          checked={tx.is_transfer}
                          disabled={busy}
                          onChange={(event) => override(tx, { is_transfer: event.target.checked })}
                          className="size-4 accent-[var(--brand-pink)] disabled:opacity-50"
                          aria-label={`mark ${tx.description} as transfer`}
                        />
                      </td>
                    </tr>
                  )
                })}
              </tbody>
            </table>
          </div>
        )}

        <div className="mt-4 flex items-center justify-between text-xs text-muted-foreground">
          <span>
            page {data?.page ?? 0} / {pages} · {total} rows
          </span>
          <div className="flex gap-2">
            <Button
              variant="outline"
              size="icon"
              disabled={page <= 1}
              onClick={() => setPage((p) => Math.max(1, p - 1))}
              aria-label="previous page"
            >
              ‹
            </Button>
            <Button
              variant="outline"
              size="icon"
              disabled={page >= pages}
              onClick={() => setPage((p) => p + 1)}
              aria-label="next page"
            >
              ›
            </Button>
          </div>
        </div>
      </CardContent>
    </Card>
  )
}

function categoryOptions(categories: Category[]) {
  return [
    { value: "all", label: "all categories" },
    ...categories.map((category) => ({ value: category.name, label: category.name })),
  ]
}

function FilterSelect({
  name,
  value,
  onChange,
  options,
}: {
  name: string
  value: string
  onChange: (value: string) => void
  options: readonly { readonly value: string; readonly label: string }[]
}) {
  return (
    <select
      name={name}
      value={value}
      onChange={(event) => onChange(event.target.value)}
      className="h-8 rounded-none border border-input bg-background px-1.5 text-xs text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
    >
      {options.map((option) => (
        <option key={option.value} value={option.value}>
          {option.label}
        </option>
      ))}
    </select>
  )
}
