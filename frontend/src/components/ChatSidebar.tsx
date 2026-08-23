import { useEffect, useRef, useState } from "react"
import { Button } from "@/components/ui/button"
import { streamChat } from "@/lib/chat"
import { cn } from "@/lib/utils"
import type { Selection } from "@/types"

type ChatItem =
  | { id: number; role: "user"; text: string }
  | {
      id: number
      role: "assistant"
      text: string
      sql: string[]
      error: string | null
      pending: boolean
    }

function SelectionChip({
  selection,
  onUnpin,
  format,
}: {
  selection: Selection
  onUnpin: (selection: Selection) => void
  format: (value: number) => string
}) {
  return (
    <span className="inline-flex max-w-full items-center gap-2 border-2 border-border bg-accent px-2 py-1 font-mono text-[11px] leading-none">
      <span className="shrink-0 uppercase tracking-widest text-muted-foreground">
        {selection.chart}
      </span>
      <span className="truncate">{selection.label}</span>
      <span className="shrink-0">{format(selection.value)}</span>
      <button
        type="button"
        aria-label={`unpin ${selection.chart} ${selection.label}`}
        onClick={() => onUnpin(selection)}
        className="shrink-0 border border-border px-1 text-xs leading-none hover:bg-background"
      >
        ×
      </button>
    </span>
  )
}

function SqlChip({ sql }: { sql: string }) {
  return (
    <div className="mb-1 font-mono text-[11px]">
      <span className="mr-1 border border-border bg-muted px-1 py-0.5 uppercase tracking-widest">
        run_sql
      </span>
      <code className="break-all text-muted-foreground">{sql}</code>
    </div>
  )
}

export function ChatSidebar({
  selections,
  onUnpin,
  onClearSelections,
  format,
}: {
  selections: Selection[]
  onUnpin: (selection: Selection) => void
  onClearSelections: () => void
  format: (value: number) => string
}) {
  const [items, setItems] = useState<ChatItem[]>([])
  const [draft, setDraft] = useState("")
  const [busy, setBusy] = useState(false)
  const idRef = useRef(0)
  const abortRef = useRef<AbortController | null>(null)
  const scrollRef = useRef<HTMLDivElement | null>(null)

  useEffect(() => {
    const el = scrollRef.current
    if (el) el.scrollTop = el.scrollHeight
  }, [items])

  // Abort an in-flight stream if the sidebar unmounts.
  useEffect(() => {
    return () => {
      abortRef.current?.abort()
    }
  }, [])

  function nextId() {
    idRef.current += 1
    return idRef.current
  }

  function patchAssistant(
    id: number,
    patch: (
      item: Extract<ChatItem, { role: "assistant" }>,
    ) => Extract<ChatItem, { role: "assistant" }>,
  ) {
    setItems((prev) =>
      prev.map((item) => (item.id === id && item.role === "assistant" ? patch(item) : item)),
    )
  }

  async function send() {
    const message = draft.trim()
    if (message === "" || busy) return
    setDraft("")
    const userItem: ChatItem = { id: nextId(), role: "user", text: message }
    const assistantId = nextId()
    setItems((prev) => [
      ...prev,
      userItem,
      {
        id: assistantId,
        role: "assistant",
        text: "",
        sql: [],
        error: null,
        pending: true,
      },
    ])
    setBusy(true)
    const controller = new AbortController()
    abortRef.current = controller
    try {
      await streamChat(
        message,
        selections,
        {
          onToken: (text) =>
            patchAssistant(assistantId, (item) => ({ ...item, text: item.text + text })),
          onTool: (sql) =>
            patchAssistant(assistantId, (item) => ({ ...item, sql: [...item.sql, sql] })),
          onError: (error) =>
            patchAssistant(assistantId, (item) => ({ ...item, error, pending: false })),
        },
        controller.signal,
      )
      patchAssistant(assistantId, (item) => ({ ...item, pending: false }))
    } catch (err) {
      const message_ = err instanceof Error ? err.message : String(err)
      patchAssistant(assistantId, (item) => ({ ...item, error: message_, pending: false }))
    } finally {
      setBusy(false)
      abortRef.current = null
    }
  }

  return (
    <aside className="flex h-dvh w-full shrink-0 flex-col border-t-2 border-border bg-background lg:sticky lg:top-0 lg:w-96 lg:border-l-2 lg:border-t-0">
      <div className="flex items-center justify-between gap-2 border-b-2 border-border px-4 py-3">
        <h2 className="font-pixel text-sm">inspector</h2>
        <div className="flex items-center gap-2">
          {selections.length > 0 && (
            <button
              type="button"
              onClick={onClearSelections}
              className="font-mono text-[11px] uppercase tracking-widest text-muted-foreground hover:text-foreground"
            >
              clear pins
            </button>
          )}
          <span className="font-mono text-[11px] uppercase tracking-widest text-muted-foreground">
            sse
          </span>
        </div>
      </div>

      {selections.length > 0 && (
        <div className="flex flex-wrap gap-1.5 border-b-2 border-border px-3 py-2">
          {selections.map((selection) => (
            <SelectionChip
              key={`${selection.chart}:${selection.label}`}
              selection={selection}
              onUnpin={onUnpin}
              format={format}
            />
          ))}
        </div>
      )}

      <div ref={scrollRef} className="flex flex-1 flex-col gap-3 overflow-y-auto px-3 py-3">
        {items.length === 0 ? (
          <div className="flex flex-1 flex-col items-center justify-center gap-2 px-4 text-center">
            <span className="font-pixel text-xs text-muted-foreground">no history</span>
            <p className="font-mono text-xs leading-relaxed text-muted-foreground">
              Click a bar, slice or point on the dashboard to pin it, then ask about your spending.
              The assistant answers from read-only SQL against the DuckDB data.
            </p>
          </div>
        ) : (
          items.map((item) =>
            item.role === "user" ? (
              <div key={item.id} className="border-2 border-border bg-accent px-2.5 py-2">
                <div className="mb-1 font-mono text-[10px] uppercase tracking-widest text-muted-foreground">
                  you
                </div>
                <p className="whitespace-pre-wrap break-words font-mono text-xs leading-relaxed">
                  {item.text}
                </p>
              </div>
            ) : (
              <div
                key={item.id}
                className={cn(
                  "border-2 px-2.5 py-2",
                  item.error !== null ? "border-destructive bg-destructive/10" : "border-border",
                )}
              >
                <div className="mb-1 font-mono text-[10px] uppercase tracking-widest text-muted-foreground">
                  assistant
                </div>
                {item.sql.map((sql, index) => (
                  <SqlChip key={index} sql={sql} />
                ))}
                {item.text !== "" && (
                  <p className="whitespace-pre-wrap break-words font-mono text-xs leading-relaxed">
                    {item.text}
                    {item.pending && <span className="animate-step-blink">▍</span>}
                  </p>
                )}
                {item.error !== null && (
                  <p className="break-words font-mono text-xs text-destructive">
                    error: {item.error}
                  </p>
                )}
                {item.pending &&
                  item.text === "" &&
                  item.error === null &&
                  item.sql.length === 0 && (
                    <p className="font-mono text-xs text-muted-foreground">
                      waiting<span className="animate-step-blink">▍</span>
                    </p>
                  )}
              </div>
            ),
          )
        )}
      </div>

      <form
        className="flex gap-2 border-t-2 border-border p-3"
        onSubmit={(event) => {
          event.preventDefault()
          void send()
        }}
      >
        <input
          name="chat"
          value={draft}
          onChange={(event) => setDraft(event.target.value)}
          placeholder={busy ? "waiting for reply…" : "ask about the pinned selection"}
          disabled={busy}
          className="h-9 min-w-0 flex-1 rounded-none border-2 border-input bg-background px-3 font-mono text-sm placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-50"
        />
        <Button type="submit" disabled={busy || draft.trim() === ""}>
          ask
        </Button>
      </form>
    </aside>
  )
}
