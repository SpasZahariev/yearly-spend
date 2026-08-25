import { cn } from "@/lib/utils"

export function Segmented<T extends string>({
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
    <div className="inline-flex overflow-hidden rounded-none border border-input">
      {options.map((option, index) => (
        <button
          key={option.value}
          type="button"
          onClick={() => onChange(option.value)}
          aria-pressed={value === option.value}
          className={cn(
            "text-xs font-medium uppercase tracking-[0.12em] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
            compact ? "h-8 px-2.5 text-[11px]" : "h-9 px-3 text-xs",
            index > 0 && "border-l border-input",
            value === option.value
              ? "bg-brand-pink-muted text-foreground hover:bg-brand-pink"
              : "bg-background text-foreground hover:bg-accent hover:text-accent-foreground",
          )}
        >
          {option.label}
        </button>
      ))}
    </div>
  )
}
