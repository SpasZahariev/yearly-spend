import { cn } from "@/lib/utils"

export function EditorialTitle({
  title,
  tag,
  className,
}: {
  title: string
  tag?: string
  className?: string
}) {
  return (
    <div className={cn("inline-flex items-stretch gap-0", className)}>
      <h2 className="font-display text-2xl font-semibold leading-none tracking-[-0.06em] sm:text-3xl">
        <span className="text-band">{title}</span>
      </h2>
      {tag !== undefined && <span className="text-tag text-tag-offset">{tag}</span>}
    </div>
  )
}
