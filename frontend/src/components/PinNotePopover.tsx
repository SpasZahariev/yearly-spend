import { useEffect, useLayoutEffect, useRef, useState } from "react"

import { Button } from "@/components/ui/button"
import type { Selection } from "@/types"

interface Anchor {
  x: number
  y: number
}

export function PinNotePopover({
  selection,
  anchor,
  initialNote,
  format,
  onSubmit,
  onCancel,
}: {
  selection: Selection
  anchor: Anchor
  initialNote: string
  format: (value: number) => string
  onSubmit: (selection: Selection, note: string) => void
  onCancel: () => void
}) {
  const rootRef = useRef<HTMLDivElement | null>(null)
  const textareaRef = useRef<HTMLTextAreaElement | null>(null)
  const [note, setNote] = useState(initialNote)
  const [position, setPosition] = useState(() => ({
    left: anchor.x + 12,
    top: anchor.y + 12,
  }))

  useEffect(() => {
    textareaRef.current?.focus()
    textareaRef.current?.setSelectionRange(initialNote.length, initialNote.length)
  }, [initialNote.length])

  useLayoutEffect(() => {
    const el = rootRef.current
    if (el === null) return
    const gap = 12
    const rect = el.getBoundingClientRect()
    const maxLeft = Math.max(gap, window.innerWidth - rect.width - gap)
    const maxTop = Math.max(gap, window.innerHeight - rect.height - gap)
    const preferredLeft = anchor.x + gap
    const preferredTop = anchor.y + gap
    const left = Math.max(gap, Math.min(preferredLeft, maxLeft))
    const top = Math.max(
      gap,
      Math.min(preferredTop > maxTop ? anchor.y - rect.height - gap : preferredTop, maxTop),
    )
    setPosition((current) =>
      current.left === left && current.top === top ? current : { left, top },
    )
  }, [anchor.x, anchor.y, note])

  useEffect(() => {
    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") onCancel()
    }
    function handlePointerDown(event: PointerEvent) {
      if (rootRef.current !== null && !rootRef.current.contains(event.target as Node)) {
        onCancel()
      }
    }
    window.addEventListener("keydown", handleKeyDown)
    document.addEventListener("pointerdown", handlePointerDown)
    return () => {
      window.removeEventListener("keydown", handleKeyDown)
      document.removeEventListener("pointerdown", handlePointerDown)
    }
  }, [onCancel])

  function submit() {
    onSubmit(selection, note.trim())
  }

  return (
    <div
      ref={rootRef}
      className="fixed z-[90] w-[min(22rem,calc(100vw-1.5rem))] border-2 border-border bg-background p-3 shadow-none"
      style={{ left: `${position.left}px`, top: `${position.top}px` }}
      role="dialog"
      aria-modal="false"
      aria-label={`Add note for ${selection.chart} ${selection.label}`}
    >
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <div className="text-[11px] font-semibold uppercase tracking-[0.12em] text-muted-foreground">
            pin note
          </div>
          <div className="mt-1 flex flex-wrap items-center gap-x-2 gap-y-1 text-sm">
            <span className="font-medium">{selection.label}</span>
            <span className="text-muted-foreground">{format(selection.value)}</span>
          </div>
        </div>
        <span className="shrink-0 border border-border bg-accent px-1.5 py-0.5 text-[10px] uppercase tracking-[0.12em] text-muted-foreground">
          {selection.chart}
        </span>
      </div>

      <textarea
        ref={textareaRef}
        value={note}
        onChange={(event) => setNote(event.target.value)}
        rows={4}
        maxLength={500}
        aria-label="Optional Notes about this feature"
        placeholder="Optional Notes about this feature"
        className="mt-3 w-full resize-none border border-input bg-background px-2.5 py-2 text-sm text-foreground outline-none focus-visible:ring-2 focus-visible:ring-ring"
      />

      <div className="mt-3 flex items-center justify-end gap-2">
        <Button type="button" variant="outline" size="sm" onClick={onCancel}>
          Cancel
        </Button>
        <Button type="button" size="sm" onClick={submit}>
          Pin
        </Button>
      </div>
    </div>
  )
}
