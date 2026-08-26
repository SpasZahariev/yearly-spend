import { useEffect, useState, type ReactNode } from "react"
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

import { EditorialTitle } from "@/components/EditorialTitle"
import { Segmented } from "@/components/Segmented"
import { SankeyDiagram, type SankeyData } from "@/components/SankeyDiagram"
import { Card, CardContent, CardDescription, CardHeader } from "@/components/ui/card"
import { getJson } from "@/lib/api"
import { MONTH_LABELS, periodLabel, type Currency, type Granularity, type View } from "@/lib/format"
import { anchorFromClick, cn } from "@/lib/utils"
import type { Category, ChartUpdate, Selection } from "@/types"

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
  sankey: SankeyData
  yearData: YearData | null
  monthData: MonthData | null
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

/**
 * Turn the chat's category values into donut slices, keeping the taxonomy
 * colors. Categories are validated against the taxonomy by the API, so the
 * color lookup only misses for unknown data.
 */
function buildChatSlices(
  entries: { name: string; value: number }[],
  taxonomy: Category[],
): CategorySlice[] {
  const colorOf = new Map(taxonomy.map((category) => [category.name, category.color]))
  const total = entries.reduce((sum, entry) => sum + entry.value, 0)
  return entries.map((entry) => ({
    name: entry.name,
    color: colorOf.get(entry.name) ?? "#c0c0c0",
    value: entry.value,
    percentage: total > 0 ? (entry.value / total) * 100 : 0,
  }))
}

type KpiMetric = "income" | "spend" | "moved"

function KpiCard({
  metric,
  value,
  period,
  currency,
  format,
  year,
  month,
  onPin,
}: {
  metric: KpiMetric
  value: number
  period: string
  currency: Currency
  format: (value: number) => string
  year: number | null
  month: number | null
  onPin: (selection: Selection, anchor: { x: number; y: number }) => void
}) {
  function pin(event: { clientX?: number; clientY?: number; currentTarget: Element }) {
    onPin(
      {
        chart: "summary",
        series: metric,
        label: `${metric} ${period}`,
        value,
        year: year ?? undefined,
        month: month ?? undefined,
      },
      anchorFromClick(event),
    )
  }

  return (
    <Card className="animate-step-in border-l-2 border-l-brand-pink">
      <CardContent className="p-6">
        <div className="inline-flex items-stretch gap-0">
          <span className="text-band text-xs font-semibold uppercase tracking-[0.08em]">
            {metric}
          </span>
          <span className="text-tag">{currency}</span>
        </div>
        <div
          role="button"
          tabIndex={0}
          aria-label={`Pin ${metric} for ${period}`}
          title={`pin ${metric} for ${period}`}
          className="mt-3 w-fit cursor-pointer rounded-sm px-1 font-display text-3xl font-semibold leading-none tracking-[-0.05em] decoration-2 underline-offset-[0.2em] outline-none transition-colors hover:bg-brand-pink/10 hover:underline focus-visible:bg-brand-pink/10 focus-visible:underline"
          onClick={(event) => pin(event)}
          onKeyDown={(event) => {
            if (event.key === "Enter" || event.key === " ") {
              event.preventDefault()
              pin(event)
            }
          }}
        >
          {format(value)}
        </div>
        <div className="mt-3 text-xs text-muted-foreground">{period}</div>
      </CardContent>
    </Card>
  )
}

const tooltipStyle = {
  border: "1px solid var(--border)",
  borderRadius: 0,
  background: "var(--background)",
  fontFamily: "Inter, sans-serif",
  fontSize: 12,
  boxShadow: "none",
}

const axisTick = { fontSize: 11, fontFamily: "Inter, sans-serif", fill: "#6d6d6d" }

function ChartLabel({ children }: { children: ReactNode }) {
  return (
    <div className="mb-1 text-[11px] font-medium uppercase tracking-[0.12em] text-muted-foreground">
      {children}
    </div>
  )
}

function YearlySpendChart({
  year,
  points,
  format,
  compact,
  onPin,
}: {
  year: number
  points: YearlyPoint[]
  format: (value: number) => string
  compact: (value: number) => string
  onPin: (selection: Selection, anchor: { x: number; y: number }) => void
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
        <Bar
          dataKey="spend"
          stroke="var(--border)"
          strokeWidth={2}
          isAnimationActive={false}
          className="cursor-pointer"
          onClick={(entry: { payload?: { year: number; spend: number } }, _index, event) => {
            const point = entry?.payload
            if (point === undefined || event === undefined) return
            onPin(
              {
                chart: "yearly",
                series: "spend",
                label: String(point.year),
                value: point.spend,
                year: point.year,
              },
              anchorFromClick(event),
            )
          }}
        >
          {data.map((point) => (
            <Cell
              key={point.year}
              fill={point.year === year ? "var(--brand-pink)" : "var(--accent)"}
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
  onPin,
}: {
  year: number
  points: CumulativePoint[]
  format: (value: number) => string
  compact: (value: number) => string
  onPin: (selection: Selection, anchor: { x: number; y: number }) => void
}) {
  const data = points.map((point) => ({
    label: MONTH_LABELS[point.month - 1],
    month: point.month,
    cumulative: point.cumulative,
  }))
  return (
    <ResponsiveContainer width="100%" height={176}>
      <LineChart
        data={data}
        margin={{ top: 8, right: 8, bottom: 0, left: 0 }}
        onClick={(state, event) => {
          if (event === undefined || typeof state.activeTooltipIndex !== "number") return
          const point = data[state.activeTooltipIndex]
          if (point === undefined) return
          onPin(
            {
              chart: "cumulative",
              series: "cumulative",
              label: `${year}-${String(point.month).padStart(2, "0")}`,
              value: point.cumulative,
              year,
              month: point.month,
            },
            anchorFromClick(event),
          )
        }}
      >
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
          stroke="var(--brand-pink)"
          strokeWidth={2}
          dot={{ r: 2.5, fill: "var(--brand-pink)", strokeWidth: 0 }}
          activeDot={{ r: 4, fill: "var(--brand-pink)", strokeWidth: 0 }}
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
  onPin,
}: {
  year: number
  months: MonthlyPoint[]
  selectedMonth: number
  format: (value: number) => string
  compact: (value: number) => string
  onPin: (selection: Selection, anchor: { x: number; y: number }) => void
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
        <Bar
          dataKey="spend"
          stroke="var(--border)"
          strokeWidth={2}
          isAnimationActive={false}
          className="cursor-pointer"
          onClick={(entry: { payload?: { month: number; spend: number } }, _index, event) => {
            const point = entry?.payload
            if (point === undefined || event === undefined) return
            onPin(
              {
                chart: "monthly",
                series: "spend",
                label: `${year}-${String(point.month).padStart(2, "0")}`,
                value: point.spend,
                year,
                month: point.month,
              },
              anchorFromClick(event),
            )
          }}
        >
          {data.map((point) => (
            <Cell
              key={point.month}
              fill={point.month === selectedMonth ? "var(--brand-pink)" : "var(--accent)"}
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
  onPin,
}: {
  year: number
  month: number
  days: DailyPoint[]
  format: (value: number) => string
  compact: (value: number) => string
  onPin: (selection: Selection, anchor: { x: number; y: number }) => void
}) {
  const data = days.map((point) => ({
    label: String(point.day),
    day: point.day,
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
          fill="var(--brand-pink)"
          stroke="var(--border)"
          strokeWidth={1}
          isAnimationActive={false}
          className="cursor-pointer"
          onClick={(entry: { payload?: { day: number; spend: number } }, _index, event) => {
            const point = entry?.payload
            if (point === undefined || event === undefined) return
            onPin(
              {
                chart: "daily",
                series: "spend",
                label: `${year}-${String(month).padStart(2, "0")}-${String(point.day).padStart(2, "0")}`,
                value: point.spend,
                year,
                month,
              },
              anchorFromClick(event),
            )
          }}
        />
      </BarChart>
    </ResponsiveContainer>
  )
}

function CategoryDonut({
  slices,
  format,
  onPin,
  year,
  month,
}: {
  slices: CategorySlice[]
  format: (value: number) => string
  onPin: (selection: Selection, anchor: { x: number; y: number }) => void
  year: number | null
  month: number | null
}) {
  const [hovered, setHovered] = useState<string | null>(null)

  if (slices.length === 0) {
    return (
      <div className="flex h-72 items-center justify-center font-mono text-sm text-muted-foreground">
        no spend recorded
      </div>
    )
  }

  function pinSlice(
    slice: CategorySlice,
    event: { clientX?: number; clientY?: number; currentTarget: Element },
  ) {
    onPin(
      {
        chart: "categories",
        series: "spend",
        label: slice.name,
        value: slice.value,
        year: year ?? undefined,
        month: month ?? undefined,
        category: slice.name,
      },
      anchorFromClick(event),
    )
  }

  return (
    <div className="flex flex-col gap-5">
      <div className="mx-auto h-64 w-full sm:h-72">
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
              className="cursor-pointer"
              onMouseEnter={(entry: { payload?: CategorySlice }) => {
                const slice = entry?.payload
                if (slice !== undefined) setHovered(slice.name)
              }}
              onMouseLeave={() => setHovered(null)}
              onClick={(entry: { payload?: CategorySlice }, _index, event) => {
                const slice = entry?.payload
                if (slice === undefined || event === undefined) return
                pinSlice(slice, event)
              }}
            >
              {slices.map((slice) => (
                <Cell
                  key={slice.name}
                  fill={slice.color}
                  fillOpacity={hovered === null || hovered === slice.name ? 1 : 0.25}
                  style={{ transition: "fill-opacity 150ms ease" }}
                />
              ))}
            </Pie>
            <Tooltip
              contentStyle={tooltipStyle}
              formatter={(value, name) => [format(Number(value)), String(name)]}
            />
          </PieChart>
        </ResponsiveContainer>
      </div>
      <ul className="grid grid-cols-[repeat(auto-fill,minmax(min(300px,100%),1fr))] gap-x-6 gap-y-1.5">
        {slices.map((slice) => {
          const isHovered = hovered === slice.name
          return (
            <li
              key={slice.name}
              tabIndex={0}
              role="button"
              aria-label={`${slice.name}, ${format(slice.value)}`}
              className={cn(
                "flex cursor-pointer flex-wrap items-center gap-2 font-mono text-sm text-foreground outline-none",
                isHovered && "font-semibold underline decoration-2 underline-offset-4",
              )}
              onMouseEnter={() => setHovered(slice.name)}
              onMouseLeave={() => setHovered(null)}
              onClick={(event) => pinSlice(slice, event)}
              onKeyDown={(event) => {
                if (event.key === "Enter" || event.key === " ") {
                  event.preventDefault()
                  pinSlice(slice, event)
                }
              }}
            >
              <span
                aria-hidden="true"
                className="size-4 shrink-0 border border-border"
                style={{ backgroundColor: slice.color }}
              />
              <span>{slice.name}</span>
              <span className="ml-auto whitespace-nowrap">{format(slice.value)}</span>
              <span className="w-12 text-right text-muted-foreground">
                {slice.percentage.toFixed(1)}%
              </span>
            </li>
          )
        })}
      </ul>
    </div>
  )
}

export interface DashboardProps {
  year: number | null
  month: number | null
  view: View
  currency: Currency
  fxReady: boolean
  error: string | null
  format: (value: number) => string
  compact: (value: number) => string
  onPin: (selection: Selection, anchor: { x: number; y: number }) => void
  reportError: (error: string | null) => void
  /**
   * Chart data pushed by the chat for the currently selected year, or
   * `null` when the chat has not pushed anything (or pushed a different
   * year). Base API data is always fetched underneath.
   */
  chartUpdate: ChartUpdate | null
  onClearChartUpdate: () => void
  /** Category taxonomy (colors) for chat-pushed donut slices. */
  taxonomy: Category[]
}

export function Dashboard({
  year,
  month,
  view,
  currency,
  fxReady,
  error,
  format,
  compact,
  onPin,
  reportError,
  chartUpdate,
  onClearChartUpdate,
  taxonomy,
}: DashboardProps) {
  const [granularity, setGranularity] = useState<Granularity>("month")
  const [data, setData] = useState<DashboardData | null>(null)

  useEffect(() => {
    if (year === null || month === null) return
    let cancelled = false
    const period = (path: string) =>
      view === "month" ? `${path}?year=${year}&month=${month}` : `${path}?year=${year}`
    Promise.all([
      getJson<Summary>(period("/api/summary")),
      getJson<CategorySlice[]>(period("/api/categories")),
      getJson<SankeyData>(period("/api/series/sankey")),
      view === "year" ? fetchYearData(year) : fetchMonthData(year, month, granularity),
    ])
      .then(([summary, slices, sankey, extra]) => {
        if (cancelled) return
        setData({
          summary,
          slices,
          sankey,
          yearData: view === "year" ? (extra as YearData) : null,
          monthData: view === "month" ? (extra as MonthData) : null,
        })
        reportError(null)
      })
      .catch((err: unknown) => {
        if (!cancelled) reportError(err instanceof Error ? err.message : String(err))
      })
    return () => {
      cancelled = true
    }
  }, [year, month, view, granularity, reportError])

  const expectedMonth = view === "month" ? month : null
  const current =
    data !== null &&
    year !== null &&
    data.summary.year === year &&
    data.summary.month === expectedMonth
      ? data
      : null
  const label = year !== null && month !== null ? periodLabel(view, year, month) : "…"

  // Chat overrides (render_dashboard) merge over the standard API data.
  // Sections the model did not send keep the base values, so a partial
  // update only rewrites the charts it was given.
  const chat = chartUpdate
  const chatKpi = chat?.kpi
  const kpi =
    current !== null && error === null
      ? {
          income: chatKpi?.income ?? current.summary.income,
          spend: chatKpi?.spend ?? current.summary.spend,
          moved: chatKpi?.moved ?? current.summary.moved,
        }
      : chatKpi
  const slices =
    chat?.categories !== undefined
      ? buildChatSlices(chat.categories, taxonomy)
      : (current?.slices ?? [])
  const sankey = current?.sankey ?? null
  const yearlyPoints =
    chat?.yearly?.map((point) => ({ year: point.year, spend: point.value })) ??
    current?.yearData?.yearly ??
    []
  const cumulativePoints =
    chat?.cumulative?.map((point) => ({ month: point.month, cumulative: point.value })) ??
    current?.yearData?.cumulative ??
    []
  const monthlyPoints =
    chat?.monthly?.map((point) => ({ month: point.month, spend: point.value })) ??
    current?.monthData?.months ??
    []
  const dailyPoints = current?.monthData?.days ?? []

  const baseReady = current !== null && error === null
  // A chart can render when its base data is ready or when the chat pushed
  // that section; the FX gate is relaxed for chat sections because their
  // values are CHF and only display conversion is pending.
  const ready = (fromChat: boolean) =>
    error === null && year !== null && (fxReady || fromChat) && (baseReady || fromChat)
  const kpiReady = ready(chatKpi !== undefined)
  const sankeyReady = ready(false)
  const yearReady = ready(chat?.yearly !== undefined || chat?.cumulative !== undefined)
  const monthReady = ready(chat?.monthly !== undefined)
  const dayReady = ready(false)
  const donutReady = ready(chat?.categories !== undefined)
  // Period labels: a chat section carries its own period label; base charts
  // keep the picker's label.
  const kpiPeriod = chat !== null && chat.kpi !== undefined ? chat.label : label
  const monthlyPeriod = chat !== null && chat.monthly !== undefined ? chat.label : label
  const categoryPeriod = chat !== null && chat.categories !== undefined ? chat.label : label

  return (
    <main className="min-w-0 flex-1 px-4 py-6 sm:px-6 sm:py-8 lg:overflow-y-auto">
      <div className="mx-auto flex w-full max-w-5xl flex-col gap-4 sm:gap-6">
        {chat !== null && (
          <div
            role="status"
            className="flex flex-wrap items-center gap-x-3 gap-y-1 border border-brand-pink bg-brand-pink/10 px-3 py-2 font-mono text-xs"
          >
            <span className="font-semibold uppercase tracking-[0.12em] text-brand-pink">
              chat data
            </span>
            <span className="text-muted-foreground">charts show {chat.label}</span>
            <button
              type="button"
              onClick={onClearChartUpdate}
              className="ml-auto border border-border px-2 py-0.5 uppercase tracking-[0.12em] hover:bg-background"
            >
              clear
            </button>
          </div>
        )}

        {kpiReady && kpi !== undefined && (
          <div className="grid grid-cols-1 gap-6 sm:grid-cols-3">
            <KpiCard
              metric="income"
              value={kpi.income}
              period={kpiPeriod}
              currency={currency}
              format={format}
              year={year}
              month={month}
              onPin={onPin}
            />
            <KpiCard
              metric="spend"
              value={kpi.spend}
              period={kpiPeriod}
              currency={currency}
              format={format}
              year={year}
              month={month}
              onPin={onPin}
            />
            <KpiCard
              metric="moved"
              value={kpi.moved}
              period={kpiPeriod}
              currency={currency}
              format={format}
              year={year}
              month={month}
              onPin={onPin}
            />
          </div>
        )}

        <div className="grid grid-cols-1 gap-4 sm:gap-6">
          <Card className="animate-step-in">
            <CardHeader>
              <EditorialTitle title="money flow" tag={String(year ?? "")} />
              <CardDescription>
                {currency} · account spend by category and paired transfers · {label}
              </CardDescription>
            </CardHeader>
            <CardContent>
              {!sankeyReady || sankey === null ? (
                <div className="flex aspect-[8/9] w-full items-center justify-center font-mono text-sm text-muted-foreground">
                  loading…
                </div>
              ) : (
                <SankeyDiagram data={sankey} format={format} onPin={onPin} />
              )}
            </CardContent>
          </Card>

          {view === "year" ? (
            <Card className="animate-step-in">
              <CardHeader>
                <EditorialTitle title="yearly spend" tag="year" />
                <CardDescription>
                  {currency} · totals per year + cumulative {year ?? "…"}
                </CardDescription>
              </CardHeader>
              <CardContent className="flex flex-col gap-5">
                {!yearReady ||
                year === null ||
                (yearlyPoints.length === 0 && cumulativePoints.length === 0) ? (
                  <div className="flex h-72 items-center justify-center font-mono text-sm text-muted-foreground">
                    loading…
                  </div>
                ) : (
                  <>
                    {yearlyPoints.length > 0 && (
                      <div>
                        <ChartLabel>total per year</ChartLabel>
                        <YearlySpendChart
                          year={year}
                          points={yearlyPoints}
                          format={format}
                          compact={compact}
                          onPin={onPin}
                        />
                      </div>
                    )}
                    {cumulativePoints.length > 0 && (
                      <div>
                        <ChartLabel>cumulative spend · {year}</ChartLabel>
                        <CumulativeChart
                          year={year}
                          points={cumulativePoints}
                          format={format}
                          compact={compact}
                          onPin={onPin}
                        />
                      </div>
                    )}
                  </>
                )}
              </CardContent>
            </Card>
          ) : (
            <Card className="animate-step-in">
              <CardHeader className="flex-row items-start justify-between gap-4">
                <div className="flex flex-col gap-1.5">
                  <EditorialTitle
                    title={granularity === "day" ? "daily spend" : "monthly spend"}
                    tag={granularity}
                  />
                  <CardDescription>
                    {currency} · {monthlyPeriod}
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
                {granularity === "month" ? (
                  month !== null && year !== null && monthReady && monthlyPoints.length > 0 ? (
                    <MonthlySpendChart
                      year={year}
                      months={monthlyPoints}
                      selectedMonth={month}
                      format={format}
                      compact={compact}
                      onPin={onPin}
                    />
                  ) : (
                    <div className="flex h-72 items-center justify-center font-mono text-sm text-muted-foreground">
                      loading…
                    </div>
                  )
                ) : month !== null && year !== null && dayReady && dailyPoints.length > 0 ? (
                  <DailySpendChart
                    year={year}
                    month={month}
                    days={dailyPoints}
                    format={format}
                    compact={compact}
                    onPin={onPin}
                  />
                ) : (
                  <div className="flex h-72 items-center justify-center font-mono text-sm text-muted-foreground">
                    loading…
                  </div>
                )}
              </CardContent>
            </Card>
          )}

          <Card className="animate-step-in">
            <CardHeader>
              <EditorialTitle title="categories" tag="spend" />
              <CardDescription>spend by category · {categoryPeriod}</CardDescription>
            </CardHeader>
            <CardContent>
              {!donutReady ? (
                <div className="flex h-72 items-center justify-center font-mono text-sm text-muted-foreground">
                  loading…
                </div>
              ) : (
                <CategoryDonut
                  slices={slices}
                  format={format}
                  onPin={onPin}
                  year={year}
                  month={month}
                />
              )}
            </CardContent>
          </Card>
        </div>
      </div>
    </main>
  )
}
