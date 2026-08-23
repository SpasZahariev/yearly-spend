import { useEffect, useRef, useState, type ReactNode } from "react"
import {
  Bar,
  BarChart,
  CartesianGrid,
  Cell,
  Line,
  LineChart,
  Pie,
  PieChart,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts"

import { Badge } from "@/components/ui/badge"
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card"
import { cn } from "@/lib/utils"

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
  month: number | null
  income: number
  spend: number
  moved: number
  net: number
}

interface MonthlyPoint {
  month: number
  spend: number
}

interface YearlyPoint {
  year: number
  spend: number
}

interface CumulativePoint {
  month: number
  cumulative: number
}

interface DailyPoint {
  day: number
  spend: number
}

interface CategorySlice {
  name: string
  color: string
  value: number
  percentage: number
}

interface YearData {
  yearly: YearlyPoint[]
  cumulative: CumulativePoint[]
}

interface MonthData {
  months: MonthlyPoint[]
  days: DailyPoint[]
}

interface DashboardData {
  summary: Summary
  slices: CategorySlice[]
  yearData: YearData | null
  monthData: MonthData | null
}

type View = "month" | "year"
type Granularity = "month" | "day"
type Currency = "CHF" | "USD" | "EUR"

interface FxState {
  rate: number
  date: string
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

const CURRENCIES: readonly Currency[] = ["CHF", "USD", "EUR"]

// currencyDisplay "code" keeps every currency rendered as `CHF 1'234.56`
// / `USD 1'234.56` / `EUR 1'234.56`, matching the pixel app's style.
const moneyFormatters: Record<Currency, Intl.NumberFormat> = {
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

const compactNumber = new Intl.NumberFormat("de-CH", {
  notation: "compact",
  maximumFractionDigits: 1,
})

function periodLabel(view: View, year: number, month: number) {
  return view === "month" ? `${MONTH_LABELS[month - 1]} ${year}` : String(year)
}

async function getJson<T>(url: string): Promise<T> {
  const res = await fetch(url)
  if (!res.ok) {
    throw new Error(`${url} responded ${res.status}`)
  }
  return (await res.json()) as T
}

function fetchYearData(year: number): Promise<YearData> {
  return Promise.all([
    getJson<YearlyPoint[]>("/api/series/yearly"),
    getJson<CumulativePoint[]>(`/api/series/cumulative?year=${year}`),
  ]).then(([yearly, cumulative]) => ({ yearly, cumulative }))
}

function fetchMonthData(year: number, month: number, granularity: Granularity): Promise<MonthData> {
  if (granularity === "month") {
    return getJson<MonthlyPoint[]>(`/api/series/monthly?year=${year}`).then((months) => ({
      months,
      days: [],
    }))
  }
  return getJson<DailyPoint[]>(`/api/series/daily?year=${year}&month=${month}`).then((days) => ({
    months: [],
    days,
  }))
}

function Segmented<T extends string>({
  options,
  value,
  onChange,
  compact = false,
}: {
  options: readonly { readonly value: T; readonly label: string }[]
  value: T
  onChange: (value: T) => void
  compact?: boolean
}) {
  return (
    <div className="inline-flex overflow-hidden rounded-none border-2 border-input">
      {options.map((option, index) => (
        <button
          key={option.value}
          type="button"
          onClick={() => onChange(option.value)}
          aria-pressed={value === option.value}
          className={cn(
            "font-mono uppercase tracking-widest focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
            compact ? "h-8 px-2.5 text-[11px]" : "h-9 px-3 text-xs",
            index > 0 && "border-l-2 border-input",
            value === option.value
              ? "bg-primary text-primary-foreground"
              : "bg-background text-foreground hover:bg-accent hover:text-accent-foreground",
          )}
        >
          {option.label}
        </button>
      ))}
    </div>
  )
}

function KpiCard({
  label,
  value,
  period,
  currency,
  format,
}: {
  label: string
  value: number
  period: string
  currency: Currency
  format: (value: number) => string
}) {
  return (
    <Card className="animate-step-in">
      <CardContent className="p-6">
        <div className="font-mono text-xs uppercase tracking-widest text-muted-foreground">
          {label}
        </div>
        <div className="mt-2 font-pixel text-3xl leading-none">{format(value)}</div>
        <div className="mt-2 font-mono text-xs text-muted-foreground">
          {currency} · {period}
        </div>
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

const axisTick = { fontSize: 11, fontFamily: "Inter, sans-serif" }

function ChartLabel({ children }: { children: ReactNode }) {
  return (
    <div className="mb-1 font-mono text-[11px] uppercase tracking-widest text-muted-foreground">
      {children}
    </div>
  )
}

function YearlySpendChart({
  year,
  points,
  format,
  compact,
}: {
  year: number
  points: YearlyPoint[]
  format: (value: number) => string
  compact: (value: number) => string
}) {
  const data = points.map((point) => ({
    label: String(point.year),
    year: point.year,
    spend: point.spend,
  }))
  return (
    <ResponsiveContainer width="100%" height={192}>
      <BarChart data={data} margin={{ top: 8, right: 8, bottom: 0, left: 0 }}>
        <CartesianGrid vertical={false} stroke="var(--muted)" />
        <XAxis
          dataKey="label"
          interval={0}
          tickLine={false}
          axisLine={{ stroke: "var(--border)", strokeWidth: 2 }}
          tick={axisTick}
        />
        <YAxis
          tickLine={false}
          axisLine={false}
          width={44}
          tickFormatter={(value: number) => compact(value)}
          tick={axisTick}
        />
        <Tooltip
          cursor={{ fill: "var(--muted)" }}
          contentStyle={tooltipStyle}
          formatter={(value) => [format(Number(value)), "spend"]}
        />
        <Bar dataKey="spend" stroke="var(--border)" strokeWidth={2} isAnimationActive={false}>
          {data.map((point) => (
            <Cell
              key={point.year}
              fill={point.year === year ? "var(--foreground)" : "var(--chart-4)"}
            />
          ))}
        </Bar>
      </BarChart>
    </ResponsiveContainer>
  )
}

function CumulativeChart({
  year,
  points,
  format,
  compact,
}: {
  year: number
  points: CumulativePoint[]
  format: (value: number) => string
  compact: (value: number) => string
}) {
  const data = points.map((point) => ({
    label: MONTH_LABELS[point.month - 1],
    cumulative: point.cumulative,
  }))
  return (
    <ResponsiveContainer width="100%" height={176}>
      <LineChart data={data} margin={{ top: 8, right: 8, bottom: 0, left: 0 }}>
        <CartesianGrid vertical={false} stroke="var(--muted)" />
        <XAxis
          dataKey="label"
          interval={0}
          tickLine={false}
          axisLine={{ stroke: "var(--border)", strokeWidth: 2 }}
          tick={axisTick}
        />
        <YAxis
          tickLine={false}
          axisLine={false}
          width={44}
          tickFormatter={(value: number) => compact(value)}
          tick={axisTick}
        />
        <Tooltip
          cursor={{ stroke: "var(--border)" }}
          contentStyle={tooltipStyle}
          formatter={(value) => [format(Number(value)), "cumulative"]}
          labelFormatter={(label) => `${label} ${year}`}
        />
        <Line
          dataKey="cumulative"
          type="stepAfter"
          stroke="var(--foreground)"
          strokeWidth={2}
          dot={{ r: 2.5, fill: "var(--foreground)", strokeWidth: 0 }}
          activeDot={{ r: 4, fill: "var(--foreground)", strokeWidth: 0 }}
          isAnimationActive={false}
        />
      </LineChart>
    </ResponsiveContainer>
  )
}

function MonthlySpendChart({
  year,
  months,
  selectedMonth,
  format,
  compact,
}: {
  year: number
  months: MonthlyPoint[]
  selectedMonth: number
  format: (value: number) => string
  compact: (value: number) => string
}) {
  const data = months.map((point) => ({
    label: MONTH_LABELS[point.month - 1],
    month: point.month,
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
          tick={axisTick}
        />
        <YAxis
          tickLine={false}
          axisLine={false}
          width={44}
          tickFormatter={(value: number) => compact(value)}
          tick={axisTick}
        />
        <Tooltip
          cursor={{ fill: "var(--muted)" }}
          contentStyle={tooltipStyle}
          formatter={(value) => [format(Number(value)), "spend"]}
          labelFormatter={(label) => `${label} ${year}`}
        />
        <Bar dataKey="spend" stroke="var(--border)" strokeWidth={2} isAnimationActive={false}>
          {data.map((point) => (
            <Cell
              key={point.month}
              fill={point.month === selectedMonth ? "var(--foreground)" : "var(--chart-4)"}
            />
          ))}
        </Bar>
      </BarChart>
    </ResponsiveContainer>
  )
}

function DailySpendChart({
  year,
  month,
  days,
  format,
  compact,
}: {
  year: number
  month: number
  days: DailyPoint[]
  format: (value: number) => string
  compact: (value: number) => string
}) {
  const data = days.map((point) => ({
    label: String(point.day),
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
          tick={{ fontSize: 9, fontFamily: "Inter, sans-serif" }}
        />
        <YAxis
          tickLine={false}
          axisLine={false}
          width={44}
          tickFormatter={(value: number) => compact(value)}
          tick={axisTick}
        />
        <Tooltip
          cursor={{ fill: "var(--muted)" }}
          contentStyle={tooltipStyle}
          formatter={(value) => [format(Number(value)), "spend"]}
          labelFormatter={(label) => `${MONTH_LABELS[month - 1]} ${label} ${year}`}
        />
        <Bar
          dataKey="spend"
          fill="var(--foreground)"
          stroke="var(--border)"
          strokeWidth={1}
          isAnimationActive={false}
        />
      </BarChart>
    </ResponsiveContainer>
  )
}

function CategoryDonut({
  slices,
  format,
}: {
  slices: CategorySlice[]
  format: (value: number) => string
}) {
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
              formatter={(value, name) => [format(Number(value)), String(name)]}
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
            <span className="ml-auto whitespace-nowrap">{format(slice.value)}</span>
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

function defaultMonthFor(year: number, periods: Period[]) {
  const months = periods.filter((period) => period.year === year).map((period) => period.month)
  return months.length > 0 ? Math.max(...months) : 12
}

export default function App() {
  const [view, setView] = useState<View>("year")
  const [granularity, setGranularity] = useState<Granularity>("month")
  const [periods, setPeriods] = useState<Period[]>([])
  const [years, setYears] = useState<number[]>([])
  const [year, setYear] = useState<number | null>(null)
  const [month, setMonth] = useState<number | null>(null)
  const [data, setData] = useState<DashboardData | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [currency, setCurrency] = useState<Currency>("CHF")
  const [fx, setFx] = useState<Partial<Record<Currency, FxState>>>({})
  const fxRequests = useRef(new Map<Currency, Promise<void>>())
  const lastGoodCurrency = useRef<Currency>("CHF")

  // One /api/fx call per currency, cached for the session: re-renders and
  // re-toggles within the same currency never hit the FX endpoint again.
  useEffect(() => {
    if (currency === "CHF") return
    if (fx[currency] !== undefined) {
      lastGoodCurrency.current = currency
      return
    }
    let request = fxRequests.current.get(currency)
    if (!request) {
      request = getJson<FxState>(`/api/fx?to=${currency}`)
        .then((state) => {
          setFx((prev) => ({ ...prev, [currency]: state }))
          lastGoodCurrency.current = currency
        })
        .catch((err: unknown) => {
          setError(`fx: ${err instanceof Error ? err.message : String(err)}`)
          setCurrency(lastGoodCurrency.current)
        })
        .finally(() => {
          fxRequests.current.delete(currency)
        })
      fxRequests.current.set(currency, request)
    }
  }, [currency, fx])

  useEffect(() => {
    let cancelled = false
    getJson<Meta>("/api/meta")
      .then((meta) => {
        if (cancelled) return
        setPeriods(meta.periods)
        const available = [...new Set(meta.periods.map((period) => period.year))].sort(
          (a, b) => a - b,
        )
        setYears(available)
        if (available.length > 0) {
          const latest = available[available.length - 1]
          setYear(latest)
          setMonth(defaultMonthFor(latest, meta.periods))
        }
      })
      .catch((err: unknown) => {
        if (!cancelled) setError(err instanceof Error ? err.message : String(err))
      })
    return () => {
      cancelled = true
    }
  }, [])

  useEffect(() => {
    if (year === null || month === null) return
    let cancelled = false
    const period = (path: string) =>
      view === "month" ? `${path}?year=${year}&month=${month}` : `${path}?year=${year}`
    Promise.all([
      getJson<Summary>(period("/api/summary")),
      getJson<CategorySlice[]>(period("/api/categories")),
      view === "year" ? fetchYearData(year) : fetchMonthData(year, month, granularity),
    ])
      .then(([summary, slices, extra]) => {
        if (cancelled) return
        setData({
          summary,
          slices,
          yearData: view === "year" ? (extra as YearData) : null,
          monthData: view === "month" ? (extra as MonthData) : null,
        })
        setError(null)
      })
      .catch((err: unknown) => {
        if (!cancelled) setError(err instanceof Error ? err.message : String(err))
      })
    return () => {
      cancelled = true
    }
  }, [year, month, view, granularity])

  function selectYear(next: number) {
    setYear(next)
    setMonth(defaultMonthFor(next, periods))
  }

  const expectedMonth = view === "month" ? month : null
  const current =
    data !== null &&
    year !== null &&
    data.summary.year === year &&
    data.summary.month === expectedMonth
      ? data
      : null
  const fxReady = currency === "CHF" || fx[currency] !== undefined
  const rate = fx[currency]?.rate ?? 1
  // CHF values come straight from the API; other currencies are converted
  // display-only, so switching back to CHF restores the originals exactly.
  const format = (value: number) => moneyFormatters[currency].format(value * rate)
  const compact = (value: number) => compactNumber.format(value * rate)
  const loading =
    year !== null && month !== null && error === null && (current === null || !fxReady)
  const label = year !== null && month !== null ? periodLabel(view, year, month) : "…"
  const summary = current?.summary ?? null
  const slices = current?.slices ?? []
  const yearData = current?.yearData ?? null
  const monthData = current?.monthData ?? null

  return (
    <div className="flex min-h-dvh flex-col bg-background text-foreground">
      <header className="pixel-rows border-b-2 border-border px-6 py-4">
        <div className="mx-auto flex w-full max-w-5xl flex-wrap items-center justify-between gap-4">
          <h1 className="font-pixel text-2xl tracking-tight">
            yearly-spend<span className="animate-step-blink">_</span>
          </h1>
          <div className="flex flex-wrap items-center gap-3">
            <Segmented
              options={CURRENCIES.map((code) => ({ value: code, label: code }))}
              value={currency}
              onChange={setCurrency}
            />
            <Segmented
              options={[
                { value: "month", label: "month" },
                { value: "year", label: "year" },
              ]}
              value={view}
              onChange={setView}
            />
            {years.length > 0 && (
              <label className="flex items-center gap-2 font-mono text-xs uppercase tracking-widest text-muted-foreground">
                year
                <select
                  value={year ?? ""}
                  onChange={(event) => selectYear(Number(event.target.value))}
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
            {view === "month" && years.length > 0 && (
              <label className="flex items-center gap-2 font-mono text-xs uppercase tracking-widest text-muted-foreground">
                month
                <select
                  value={month ?? ""}
                  onChange={(event) => setMonth(Number(event.target.value))}
                  className="h-9 rounded-none border-2 border-input bg-background px-2 font-mono text-sm text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                >
                  {MONTH_LABELS.map((name, index) => (
                    <option key={name} value={index + 1}>
                      {name}
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
            {current !== null && fxReady && (
              <div className="grid grid-cols-1 gap-6 sm:grid-cols-3">
                <KpiCard
                  label="income"
                  value={current.summary.income}
                  period={label}
                  currency={currency}
                  format={format}
                />
                <KpiCard
                  label="spend"
                  value={current.summary.spend}
                  period={label}
                  currency={currency}
                  format={format}
                />
                <KpiCard
                  label="moved"
                  value={current.summary.moved}
                  period={label}
                  currency={currency}
                  format={format}
                />
              </div>
            )}

            <div className="grid grid-cols-1 gap-6 lg:grid-cols-2">
              {view === "year" ? (
                <Card className="animate-step-in">
                  <CardHeader>
                    <CardTitle className="text-lg">yearly spend</CardTitle>
                    <CardDescription>
                      {currency} · totals per year + cumulative {year ?? "…"}
                    </CardDescription>
                  </CardHeader>
                  <CardContent className="flex flex-col gap-5">
                    {loading || year === null || yearData === null ? (
                      <div className="flex h-72 items-center justify-center font-mono text-sm text-muted-foreground">
                        loading…
                      </div>
                    ) : (
                      <>
                        <div>
                          <ChartLabel>total per year</ChartLabel>
                          <YearlySpendChart
                            year={year}
                            points={yearData.yearly}
                            format={format}
                            compact={compact}
                          />
                        </div>
                        <div>
                          <ChartLabel>cumulative spend · {year}</ChartLabel>
                          <CumulativeChart
                            year={year}
                            points={yearData.cumulative}
                            format={format}
                            compact={compact}
                          />
                        </div>
                      </>
                    )}
                  </CardContent>
                </Card>
              ) : (
                <Card className="animate-step-in">
                  <CardHeader className="flex-row items-start justify-between gap-4">
                    <div className="flex flex-col gap-1.5">
                      <CardTitle className="text-lg">
                        {granularity === "day" ? "daily spend" : "monthly spend"}
                      </CardTitle>
                      <CardDescription>
                        {currency} · {label}
                      </CardDescription>
                    </div>
                    <Segmented
                      compact
                      options={[
                        { value: "month", label: "month" },
                        { value: "day", label: "day" },
                      ]}
                      value={granularity}
                      onChange={setGranularity}
                    />
                  </CardHeader>
                  <CardContent>
                    {loading || year === null || month === null || monthData === null ? (
                      <div className="flex h-72 items-center justify-center font-mono text-sm text-muted-foreground">
                        loading…
                      </div>
                    ) : granularity === "month" ? (
                      <MonthlySpendChart
                        year={year}
                        months={monthData.months}
                        selectedMonth={month}
                        format={format}
                        compact={compact}
                      />
                    ) : (
                      <DailySpendChart
                        year={year}
                        month={month}
                        days={monthData.days}
                        format={format}
                        compact={compact}
                      />
                    )}
                  </CardContent>
                </Card>
              )}

              <Card className="animate-step-in">
                <CardHeader>
                  <CardTitle className="text-lg">categories</CardTitle>
                  <CardDescription>spend by category · {label}</CardDescription>
                </CardHeader>
                <CardContent>
                  {loading || summary === null ? (
                    <div className="flex h-72 items-center justify-center font-mono text-sm text-muted-foreground">
                      loading…
                    </div>
                  ) : (
                    <CategoryDonut slices={slices} format={format} />
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
