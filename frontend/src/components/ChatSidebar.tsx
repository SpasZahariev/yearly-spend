import { useEffect, useRef, useState } from "react"
import { Button } from "@/components/ui/button"
import { streamChat } from "@/lib/chat"
import { cn } from "@/lib/utils"
import type { Selection } from "@/types"

const STARTER_PROMPTS = [
  "What was my biggest spending category this year?",
  "Which account spent the most this year?",
  "How much did I move between accounts?",
  "What were my three most expensive months?",
]

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
    <span className="inline-flex max-w-full items-center gap-2 border border-border bg-accent px-2 py-1 text-[11px] leading-none">
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
      <span className="mr-1 border border-border bg-muted px-1 py-0.5 text-[10px] font-medium uppercase tracking-[0.12em]">
        run_sql
      </span>
      <code className="break-all text-muted-foreground">{sql}</code>
    </div>
  )
}

export function ChatSidebar({
  open,
  selections,
  onUnpin,
  onClearSelections,
  format,
}: {
  open: boolean
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

  async function send(nextMessage = draft) {
    const message = nextMessage.trim()
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
    <aside
      aria-label="AI chat"
      aria-hidden={!open}
      inert={!open}
      className={cn(
        "flex shrink-0 flex-col overflow-hidden bg-background transition-[width,height,opacity] duration-200 ease-out",
        open
          ? "h-[70dvh] w-full border-t border-border opacity-100 lg:h-auto lg:w-96 lg:border-l lg:border-t-0"
          : "pointer-events-none h-0 w-full border-0 opacity-0 lg:h-auto lg:w-0",
      )}
    >
      {selections.length > 0 && (
        <div className="flex items-center justify-end gap-2 border-b border-border px-4 py-3">
          <button
            type="button"
            onClick={onClearSelections}
            className="text-[11px] font-medium uppercase tracking-[0.12em] text-muted-foreground hover:text-foreground"
          >
            clear pins
          </button>
        </div>
      )}

      {selections.length > 0 && (
        <div className="flex flex-wrap gap-1.5 border-b border-border px-3 py-2">
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
          <div className="flex flex-1 flex-col items-center justify-center gap-4 px-4 text-center">
            <span className="font-display text-xs font-bold text-muted-foreground">
              start with a question
            </span>
            <p className="font-mono text-xs leading-relaxed text-muted-foreground">
              Click a suggestion or pin a bar, slice or point on the dashboard. The assistant
              answers from read-only SQL against the DuckDB data.
            </p>
            <div className="flex w-full max-w-[300px] flex-col gap-2">
              {STARTER_PROMPTS.map((prompt) => (
                <button
                  key={prompt}
                  type="button"
                  disabled={busy}
                  onClick={() => void send(prompt)}
                  className="border border-foreground bg-foreground px-3 py-2 text-left text-xs leading-relaxed text-background transition-opacity hover:opacity-80 disabled:pointer-events-none disabled:opacity-50"
                >
                  {prompt}
                </button>
              ))}
            </div>
          </div>
        ) : (
          items.map((item) =>
            item.role === "user" ? (
              <div key={item.id} className="border border-border bg-accent px-2.5 py-2">
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
                  "border px-2.5 py-2",
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
        className="flex gap-2 border-t border-border p-3"
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
          className="h-10 min-w-0 flex-1 rounded-none border border-input bg-background px-3 text-sm placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-50"
        />
        <Button type="submit" disabled={busy || draft.trim() === ""}>
          ask
        </Button>
      </form>
    </aside>
  )
}
