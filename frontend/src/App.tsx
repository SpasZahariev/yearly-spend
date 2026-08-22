import { useEffect, useState } from "react"
import {
  Bar,
  BarChart,
  CartesianGrid,
  Cell,
  Pie,
  PieChart,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts"

import { Badge } from "@/components/ui/badge"
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card"

interface Account {
  id: number
  source: string | null
  name: string
  currency: string | null
  is_internal: boolean
}

interface Category {
  id: number
  name: string
  color: string
}

interface Period {
  year: number
  month: number
}

interface Meta {
  accounts: Account[]
  categories: Category[]
  periods: Period[]
}

interface Summary {
  year: number
  income: number
  spend: number
  moved: number
  net: number
}

interface MonthlyPoint {
  month: number
  spend: number
}

interface CategorySlice {
  name: string
  color: string
  value: number
  percentage: number
}

interface Dashboard {
  summary: Summary
  months: MonthlyPoint[]
  slices: CategorySlice[]
}

const MONTH_LABELS = [
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

const chf = new Intl.NumberFormat("de-CH", {
  style: "currency",
  currency: "CHF",
})

const chfCompact = new Intl.NumberFormat("de-CH", {
  notation: "compact",
  maximumFractionDigits: 1,
})

function formatChf(value: number) {
  return chf.format(value)
}

async function getJson<T>(url: string): Promise<T> {
  const res = await fetch(url)
  if (!res.ok) {
    throw new Error(`${url} responded ${res.status}`)
  }
  return (await res.json()) as T
}

function KpiCard({ label, value, year }: { label: string; value: number; year: number }) {
  return (
    <Card className="animate-step-in">
      <CardContent className="p-6">
        <div className="font-mono text-xs uppercase tracking-widest text-muted-foreground">
          {label}
        </div>
        <div className="mt-2 font-pixel text-3xl leading-none">{formatChf(value)}</div>
        <div className="mt-2 font-mono text-xs text-muted-foreground">CHF · {year}</div>
      </CardContent>
    </Card>
  )
}

const tooltipStyle = {
  border: "2px solid var(--border)",
  borderRadius: 0,
  background: "var(--background)",
  fontFamily: "ui-monospace, monospace",
  fontSize: 12,
  boxShadow: "4px 4px 0 var(--border)",
}

function MonthlySpendChart({ year, months }: { year: number; months: MonthlyPoint[] }) {
  const data = months.map((point) => ({
    label: MONTH_LABELS[point.month - 1],
    spend: point.spend,
  }))
  return (
    <ResponsiveContainer width="100%" height={288}>
      <BarChart data={data} margin={{ top: 8, right: 8, bottom: 0, left: 0 }}>
        <CartesianGrid vertical={false} stroke="var(--muted)" />
        <XAxis
          dataKey="label"
          interval={0}
          tickLine={false}
          axisLine={{ stroke: "var(--border)", strokeWidth: 2 }}
          tick={{ fontSize: 11, fontFamily: "Inter, sans-serif" }}
        />
        <YAxis
          tickLine={false}
          axisLine={false}
          width={44}
          tickFormatter={(value: number) => chfCompact.format(value)}
          tick={{ fontSize: 11, fontFamily: "Inter, sans-serif" }}
        />
        <Tooltip
          cursor={{ fill: "var(--muted)" }}
          contentStyle={tooltipStyle}
          formatter={(value) => [formatChf(Number(value)), "spend"]}
          labelFormatter={(label) => `${label} ${year}`}
        />
        <Bar
          dataKey="spend"
          fill="var(--foreground)"
          stroke="var(--border)"
          strokeWidth={2}
          isAnimationActive={false}
        />
      </BarChart>
    </ResponsiveContainer>
  )
}

function CategoryDonut({ slices }: { slices: CategorySlice[] }) {
  if (slices.length === 0) {
    return (
      <div className="flex h-72 items-center justify-center font-mono text-sm text-muted-foreground">
        no spend recorded
      </div>
    )
  }
  return (
    <div className="flex flex-col gap-4">
      <div className="h-56">
        <ResponsiveContainer width="100%" height="100%">
          <PieChart>
            <Pie
              data={slices}
              dataKey="value"
              nameKey="name"
              innerRadius="58%"
              outerRadius="95%"
              paddingAngle={0}
              stroke="var(--border)"
              strokeWidth={2}
              isAnimationActive={false}
            >
              {slices.map((slice) => (
                <Cell key={slice.name} fill={slice.color} />
              ))}
            </Pie>
            <Tooltip
              contentStyle={tooltipStyle}
              formatter={(value, name) => [formatChf(Number(value)), String(name)]}
            />
          </PieChart>
        </ResponsiveContainer>
      </div>
      <ul className="grid grid-cols-1 gap-x-6 gap-y-1 sm:grid-cols-2">
        {slices.map((slice) => (
          <li
            key={slice.name}
            className="flex items-center gap-2 font-mono text-xs text-foreground"
          >
            <span
              aria-hidden="true"
              className="size-3 shrink-0 border-2 border-border"
              style={{ backgroundColor: slice.color }}
            />
            <span className="truncate">{slice.name}</span>
            <span className="ml-auto whitespace-nowrap">{formatChf(slice.value)}</span>
            <span className="w-12 text-right text-muted-foreground">
              {slice.percentage.toFixed(1)}%
            </span>
          </li>
        ))}
      </ul>
    </div>
  )
}

function EmptyState() {
  return (
    <main className="flex flex-1 items-center justify-center px-6 py-16">
      <Card className="w-full max-w-lg animate-step-in">
        <CardHeader>
          <CardTitle>nothing ingested yet</CardTitle>
          <CardDescription>
            Statements under <code className="font-mono text-xs">statements/</code> are parsed by
            the <code className="font-mono text-xs">spend</code> CLI into DuckDB. The dashboard
            fills in once data exists.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <code className="font-mono text-xs">spend ingest statements/</code>
        </CardContent>
      </Card>
    </main>
  )
}

export default function App() {
  const [years, setYears] = useState<number[]>([])
  const [year, setYear] = useState<number | null>(null)
  const [dashboard, setDashboard] = useState<Dashboard | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    let cancelled = false
    getJson<Meta>("/api/meta")
      .then((meta) => {
        if (cancelled) return
        const available = [...new Set(meta.periods.map((period) => period.year))].sort(
          (a, b) => a - b,
        )
        setYears(available)
        setYear(available.length > 0 ? available[available.length - 1] : null)
      })
      .catch((err: unknown) => {
        if (!cancelled) setError(err instanceof Error ? err.message : String(err))
      })
    return () => {
      cancelled = true
    }
  }, [])

  useEffect(() => {
    if (year === null) return
    let cancelled = false
    Promise.all([
      getJson<Summary>(`/api/summary?year=${year}`),
      getJson<MonthlyPoint[]>(`/api/series/monthly?year=${year}`),
      getJson<CategorySlice[]>(`/api/categories?year=${year}`),
    ])
      .then(([summary, months, slices]) => {
        if (!cancelled) setDashboard({ summary, months, slices })
      })
      .catch((err: unknown) => {
        if (!cancelled) setError(err instanceof Error ? err.message : String(err))
      })
    return () => {
      cancelled = true
    }
  }, [year])

  const current =
    dashboard !== null && year !== null && dashboard.summary.year === year ? dashboard : null
  const chartMonths = current?.months ?? []
  const slices = current?.slices ?? []
  const loading = year !== null && error === null && current === null

  return (
    <div className="flex min-h-dvh flex-col bg-background text-foreground">
      <header className="pixel-rows border-b-2 border-border px-6 py-4">
        <div className="mx-auto flex w-full max-w-5xl items-center justify-between gap-4">
          <h1 className="font-pixel text-2xl tracking-tight">
            yearly-spend<span className="animate-step-blink">_</span>
          </h1>
          <div className="flex items-center gap-3">
            {years.length > 0 && (
              <label className="flex items-center gap-2 font-mono text-xs uppercase tracking-widest text-muted-foreground">
                year
                <select
                  value={year ?? ""}
                  onChange={(event) => setYear(Number(event.target.value))}
                  className="h-9 rounded-none border-2 border-input bg-background px-2 font-mono text-sm text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                >
                  {years.map((option) => (
                    <option key={option} value={option}>
                      {option}
                    </option>
                  ))}
                </select>
              </label>
            )}
            <Badge variant="outline">local</Badge>
          </div>
        </div>
      </header>

      <div className="dither-band" aria-hidden="true" />

      {error !== null && (
        <div className="border-b-2 border-border bg-destructive px-6 py-2 font-mono text-sm text-destructive-foreground">
          error: {error}
        </div>
      )}

      {year === null && error === null && years.length === 0 ? (
        <EmptyState />
      ) : (
        <main className="flex-1 px-6 py-8">
          <div className="mx-auto flex w-full max-w-5xl flex-col gap-6">
            {year !== null && (
              <div className="grid grid-cols-1 gap-6 sm:grid-cols-2">
                <KpiCard label="income" value={current?.summary.income ?? 0} year={year} />
                <KpiCard label="spend" value={current?.summary.spend ?? 0} year={year} />
              </div>
            )}

            <div className="grid grid-cols-1 gap-6 lg:grid-cols-2">
              <Card className="animate-step-in">
                <CardHeader>
                  <CardTitle className="text-lg">monthly spend</CardTitle>
                  <CardDescription>CHF · {year ?? "…"}</CardDescription>
                </CardHeader>
                <CardContent>
                  {loading || year === null ? (
                    <div className="flex h-72 items-center justify-center font-mono text-sm text-muted-foreground">
                      loading…
                    </div>
                  ) : (
                    <MonthlySpendChart year={year} months={chartMonths} />
                  )}
                </CardContent>
              </Card>

              <Card className="animate-step-in">
                <CardHeader>
                  <CardTitle className="text-lg">categories</CardTitle>
                  <CardDescription>spend by category · {year ?? "…"}</CardDescription>
                </CardHeader>
                <CardContent>
                  {loading || year === null ? (
                    <div className="flex h-72 items-center justify-center font-mono text-sm text-muted-foreground">
                      loading…
                    </div>
                  ) : (
                    <CategoryDonut slices={slices} />
                  )}
                </CardContent>
              </Card>
            </div>
          </div>
        </main>
      )}

      <footer className="border-t-2 border-border px-6 py-3">
        <div className="mx-auto flex w-full max-w-5xl items-center justify-between text-xs text-muted-foreground">
          <span className="font-mono">duckdb · axum · react</span>
          <span>local only</span>
        </div>
      </footer>
    </div>
  )
}
