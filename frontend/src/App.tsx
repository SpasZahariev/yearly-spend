import { useEffect, useRef, useState, type KeyboardEvent } from "react"
import { ChevronDown, MessageCircle } from "lucide-react"

import { ChatSidebar } from "@/components/ChatSidebar"
import { EmptyState } from "@/components/EmptyState"
import { Segmented } from "@/components/Segmented"
import { getJson } from "@/lib/api"
import {
  CURRENCIES,
  MONTH_LABELS,
  compactNumber,
  defaultMonthFor,
  moneyFormatters,
  type Currency,
  type FxState,
  type Period,
  type View,
} from "@/lib/format"
import { cn } from "@/lib/utils"
import { Dashboard } from "@/pages/Dashboard"
import { TransactionsPage } from "@/pages/Transactions"
import { selectionKey, type Category, type Selection } from "@/types"

interface Account {
  id: number
  source: string | null
  name: string
  currency: string | null
  is_internal: boolean
}

interface Meta {
  accounts: Account[]
  categories: Category[]
  periods: Period[]
}

type Route = "dashboard" | "transactions"

// Two hash routes: `#/` (dashboard) and `#/transactions` (transactions
// table). Hash routing keeps the SPA fallback and back/forward buttons
// working without a router dependency.
function parseRoute(hash: string): Route {
  return hash.startsWith("#/transactions") ? "transactions" : "dashboard"
}

function useHashRoute() {
  const [route, setRoute] = useState<Route>(() => parseRoute(window.location.hash))
  useEffect(() => {
    const onHashChange = () => {
      setRoute(parseRoute(window.location.hash))
      window.scrollTo(0, 0)
    }
    window.addEventListener("hashchange", onHashChange)
    return () => window.removeEventListener("hashchange", onHashChange)
  }, [])
  return route
}

function Picker<T extends string | number>({
  value,
  options,
  onChange,
  label,
  className,
}: {
  value: T
  options: readonly { value: T; label: string }[]
  onChange: (value: T) => void
  label: string
  className?: string
}) {
  const [open, setOpen] = useState(false)
  const rootRef = useRef<HTMLDivElement | null>(null)
  const selectedIndex = Math.max(
    0,
    options.findIndex((option) => option.value === value),
  )

  useEffect(() => {
    function closeOnOutsidePointer(event: PointerEvent) {
      if (rootRef.current !== null && !rootRef.current.contains(event.target as Node)) {
        setOpen(false)
      }
    }
    document.addEventListener("pointerdown", closeOnOutsidePointer)
    return () => document.removeEventListener("pointerdown", closeOnOutsidePointer)
  }, [])

  function choose(next: T) {
    onChange(next)
    setOpen(false)
  }

  function handleKeyDown(event: KeyboardEvent<HTMLButtonElement>) {
    if (event.key === "Escape") {
      setOpen(false)
      return
    }
    if (event.key === " " || event.key === "Enter") {
      event.preventDefault()
      setOpen((current) => !current)
      return
    }
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return
    event.preventDefault()
    const delta = event.key === "ArrowDown" ? 1 : -1
    const nextIndex = Math.max(0, Math.min(options.length - 1, selectedIndex + delta))
    if (open) choose(options[nextIndex].value)
    else setOpen(true)
  }

  return (
    <div ref={rootRef} className={cn("relative", className)}>
      <button
        type="button"
        aria-label={label}
        aria-haspopup="listbox"
        aria-expanded={open}
        onClick={() => setOpen((current) => !current)}
        onKeyDown={handleKeyDown}
        className="relative flex h-10 w-full items-center border border-input bg-background px-2.5 pr-8 text-sm text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
      >
        <span className="truncate">{options[selectedIndex]?.label ?? value}</span>
        <ChevronDown
          size={14}
          strokeWidth={2}
          aria-hidden="true"
          className={cn(
            "pointer-events-none absolute right-2.5 transition-transform",
            open && "rotate-180",
          )}
        />
      </button>
      {open && (
        <div
          role="listbox"
          aria-label={label}
          className="absolute right-0 top-[calc(100%+1px)] z-50 min-w-full border border-input bg-background shadow-none"
        >
          {options.map((option) => (
            <button
              key={String(option.value)}
              type="button"
              role="option"
              aria-selected={option.value === value}
              onClick={() => choose(option.value)}
              className={cn(
                "block w-full whitespace-nowrap px-2.5 py-2 text-left text-sm text-foreground hover:bg-accent focus-visible:bg-accent focus-visible:outline-none",
                option.value === value && "bg-brand-pink-muted",
              )}
            >
              {option.label}
            </button>
          ))}
        </div>
      )}
    </div>
  )
}

export default function App() {
  const route = useHashRoute()
  const [view, setView] = useState<View>("year")
  const [periods, setPeriods] = useState<Period[]>([])
  const [years, setYears] = useState<number[]>([])
  const [year, setYear] = useState<number | null>(null)
  const [month, setMonth] = useState<number | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [taxonomy, setTaxonomy] = useState<Category[]>([])
  const [currency, setCurrency] = useState<Currency>("CHF")
  const [fx, setFx] = useState<Partial<Record<Currency, FxState>>>({})
  const [selections, setSelections] = useState<Selection[]>([])
  const [chatOpen, setChatOpen] = useState(true)
  const fxRequests = useRef(new Map<Currency, Promise<void>>())
  const lastGoodCurrency = useRef<Currency>("CHF")

  // Pins are per-tab state, never persisted. A chart can contribute multiple
  // pins, but clicking the same chart element again toggles its existing pin.
  function pinSelection(next: Selection) {
    setSelections((prev) => {
      const key = selectionKey(next)
      const existingIndex = prev.findIndex((selection) => selectionKey(selection) === key)
      if (existingIndex !== -1) {
        return prev.filter((_, index) => index !== existingIndex)
      }
      return [...prev, next]
    })
  }

  function unpinSelection(target: Selection) {
    setSelections((prev) =>
      prev.filter((selection) => selectionKey(selection) !== selectionKey(target)),
    )
  }

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
        setTaxonomy(meta.categories)
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

  function selectYear(next: number) {
    setYear(next)
    setMonth(defaultMonthFor(next, periods))
  }

  const fxReady = currency === "CHF" || fx[currency] !== undefined
  const rate = fx[currency]?.rate ?? 1
  // CHF values come straight from the API; other currencies are converted
  // display-only, so switching back to CHF restores the originals exactly.
  const format = (value: number) => moneyFormatters[currency].format(value * rate)
  const compact = (value: number) => compactNumber.format(value * rate)

  return (
    <div className="app-frame flex flex-col overflow-hidden text-foreground">
      <header className="pixel-rows border-b border-border px-4">
        <div className="flex min-h-16 w-full flex-wrap items-center justify-between gap-3">
          <div className="flex items-center gap-3">
            <span className="brand-mark" aria-hidden="true">
              ys
            </span>
            <h1 className="font-display text-xl font-semibold tracking-[-0.05em] sm:text-2xl">
              yearly-spend<span className="animate-step-blink text-brand-pink">_</span>
            </h1>
          </div>
          <div className="flex flex-wrap items-center gap-2">
            <Segmented
              options={[
                { value: "dashboard", label: "dashboard" },
                { value: "transactions", label: "transactions" },
              ]}
              value={route}
              onChange={(next) => {
                window.location.hash = next === "transactions" ? "/transactions" : "/"
              }}
            />
            <Segmented
              options={CURRENCIES.map((code) => ({ value: code, label: code }))}
              value={currency}
              onChange={setCurrency}
            />
            {route === "dashboard" && (
              <Segmented
                options={[
                  { value: "month", label: "month" },
                  { value: "year", label: "year" },
                ]}
                value={view}
                onChange={setView}
              />
            )}
            {years.length > 0 && (
              <Picker
                value={year ?? years[0]}
                options={years.map((option) => ({ value: option, label: String(option) }))}
                onChange={selectYear}
                label="year"
                className="w-[90px] shrink-0"
              />
            )}
            {route === "dashboard" && years.length > 0 && (
              <div className="flex h-10 w-[90px] shrink-0 items-center">
                {view === "month" ? (
                  <Picker
                    value={month ?? 1}
                    options={MONTH_LABELS.map((name, index) => ({
                      value: index + 1,
                      label: name,
                    }))}
                    onChange={setMonth}
                    label="month"
                    className="w-full"
                  />
                ) : (
                  <div aria-hidden="true" className="h-10 w-full" />
                )}
              </div>
            )}
            <button
              type="button"
              aria-label={chatOpen ? "collapse AI chat" : "expand AI chat"}
              aria-expanded={chatOpen}
              title={chatOpen ? "collapse AI chat" : "expand AI chat"}
              onClick={() => setChatOpen((open) => !open)}
              className="flex size-10 items-center justify-center border-0 bg-brand-pink text-foreground transition-colors hover:bg-foreground hover:text-brand-pink focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            >
              <MessageCircle size={18} strokeWidth={2.25} aria-hidden="true" />
            </button>
          </div>
        </div>
      </header>

      <div className="dither-band" aria-hidden="true" />

      {error !== null && (
        <div className="border-b border-border bg-destructive px-4 py-2 text-sm text-destructive-foreground">
          error: {error}
        </div>
      )}

      <div className="flex flex-1 flex-col lg:min-h-0 lg:flex-row">
        {route === "transactions" ? (
          <TransactionsPage year={year} categories={taxonomy} format={format} />
        ) : year === null && error === null && years.length === 0 ? (
          <EmptyState />
        ) : (
          <Dashboard
            year={year}
            month={month}
            view={view}
            currency={currency}
            fxReady={fxReady}
            error={error}
            format={format}
            compact={compact}
            onPin={pinSelection}
            reportError={setError}
          />
        )}
        {route === "dashboard" && (
          <ChatSidebar
            open={chatOpen}
            selections={selections}
            onUnpin={unpinSelection}
            onClearSelections={() => setSelections([])}
            format={format}
          />
        )}
      </div>

      <footer className="border-t border-border px-4 py-3">
        <div className="flex w-full items-center justify-between text-xs text-muted-foreground">
          <span>duckdb · axum · react</span>
          <span>local only</span>
        </div>
      </footer>
    </div>
  )
}
