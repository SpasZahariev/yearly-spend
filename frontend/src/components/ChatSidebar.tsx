import { useEffect, useRef, useState, type ReactNode } from "react"
import { Button } from "@/components/ui/button"
import { streamChat } from "@/lib/chat"
import { cn } from "@/lib/utils"
import { selectionKey, type ChartUpdate, type Selection } from "@/types"

const STARTER_PROMPTS = [
  "What was my biggest spending category this year?",
  "Which account spent the most this year?",
  "How much did I move between accounts?",
]

type ChatItem =
  | { id: number; role: "user"; text: string }
  | {
      id: number
      role: "assistant"
      text: string
      sql: string[]
      /** Period labels of the dashboard updates this reply pushed. */
      charts: string[]
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
    <span className="inline-flex max-w-full items-start gap-2 border border-border bg-accent px-2 py-1 text-[11px] leading-none">
      <span className="min-w-0 flex-1">
        <span className="flex flex-wrap items-center gap-x-2 gap-y-1">
          <span className="shrink-0 uppercase tracking-widest text-muted-foreground">
            {selection.chart}
          </span>
          <span className="truncate">{selection.label}</span>
          <span className="shrink-0">{format(selection.value)}</span>
        </span>
        {selection.note !== undefined && (
          <span className="mt-1 block whitespace-pre-wrap break-words leading-relaxed text-muted-foreground">
            {selection.note}
          </span>
        )}
      </span>
      <button
        type="button"
        aria-label={`unpin ${selection.chart} ${selection.label}`}
        onClick={() => onUnpin(selection)}
        className="mt-px shrink-0 border border-border px-1 text-xs leading-none hover:bg-background"
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

function ChartChip({ label }: { label: string }) {
  return (
    <div className="mb-1 font-mono text-[11px]">
      <span className="mr-1 border border-brand-pink bg-brand-pink/10 px-1 py-0.5 text-[10px] font-medium uppercase tracking-[0.12em]">
        dashboard
      </span>
      <span className="text-muted-foreground">updated · {label}</span>
    </div>
  )
}

function safeHref(href: string) {
  const value = href.trim()
  return /^(https?:|mailto:)/i.test(value) ? value : null
}

function renderInline(text: string, keyPrefix: string): ReactNode[] {
  const tokenPattern =
    /`([^`\n]+)`|\[([^\]]+)\]\(([^)\s]+)(?:\s+["'][^)]*)?\)|\*\*([^*\n]+)\*\*|__([^_\n]+)__|~~([^~\n]+)~~|\*([^*\n]+)\*|_([^_\n]+)_|(https?:\/\/[^\s<]+)/g
  const nodes: ReactNode[] = []
  let cursor = 0
  let match: RegExpExecArray | null
  let tokenIndex = 0

  while ((match = tokenPattern.exec(text)) !== null) {
    if (match.index > cursor) {
      nodes.push(text.slice(cursor, match.index))
    }

    if (match[1] !== undefined) {
      nodes.push(
        <code
          key={`${keyPrefix}-code-${tokenIndex}`}
          className="border border-border bg-muted px-1"
        >
          {match[1]}
        </code>,
      )
    } else if (match[2] !== undefined && match[3] !== undefined) {
      const href = safeHref(match[3])
      nodes.push(
        href === null ? (
          <span key={`${keyPrefix}-link-${tokenIndex}`}>{match[2]}</span>
        ) : (
          <a
            key={`${keyPrefix}-link-${tokenIndex}`}
            href={href}
            target="_blank"
            rel="noreferrer"
            className="text-brand-pink underline decoration-brand-pink underline-offset-2 hover:text-foreground"
          >
            {match[2]}
          </a>
        ),
      )
    } else if (match[4] !== undefined || match[5] !== undefined) {
      nodes.push(
        <strong key={`${keyPrefix}-strong-${tokenIndex}`}>
          {renderInline(match[4] ?? match[5] ?? "", `${keyPrefix}-strong-${tokenIndex}`)}
        </strong>,
      )
    } else if (match[6] !== undefined) {
      nodes.push(
        <del key={`${keyPrefix}-del-${tokenIndex}`}>
          {renderInline(match[6], `${keyPrefix}-del-${tokenIndex}`)}
        </del>,
      )
    } else if (match[7] !== undefined || match[8] !== undefined) {
      nodes.push(
        <em key={`${keyPrefix}-em-${tokenIndex}`}>
          {renderInline(match[7] ?? match[8] ?? "", `${keyPrefix}-em-${tokenIndex}`)}
        </em>,
      )
    } else if (match[9] !== undefined) {
      const rawHref = match[9]
      const href = rawHref.replace(/[),.!?;:]+$/, "")
      const trailing = rawHref.slice(href.length)
      nodes.push(
        <a
          key={`${keyPrefix}-url-${tokenIndex}`}
          href={href}
          target="_blank"
          rel="noreferrer"
          className="text-brand-pink underline decoration-brand-pink underline-offset-2 hover:text-foreground"
        >
          {href}
        </a>,
      )
      if (trailing !== "") nodes.push(trailing)
    }

    cursor = match.index + match[0].length
    tokenIndex += 1
  }

  if (cursor < text.length) nodes.push(text.slice(cursor))
  return nodes
}

function tableCells(line: string) {
  const trimmed = line.trim().replace(/^\|/, "").replace(/\|$/, "")
  return trimmed.split("|").map((cell) => cell.trim())
}

function isTableDivider(line: string) {
  return tableCells(line).every((cell) => /^:?-{3,}:?$/.test(cell))
}

function renderMarkdownBlocks(markdown: string, keyPrefix: string): ReactNode[] {
  const lines = markdown.replace(/\r\n?/g, "\n").split("\n")
  const blocks: ReactNode[] = []
  let index = 0
  let blockIndex = 0

  while (index < lines.length) {
    if (lines[index].trim() === "") {
      index += 1
      continue
    }

    const blockKey = `${keyPrefix}-${blockIndex}`
    const fence = lines[index].match(/^ {0,3}(```+|~~~+)\s*(.*)$/)
    if (fence !== null) {
      const marker = fence[1][0]
      const code: string[] = []
      const language = fence[2].trim()
      index += 1
      while (index < lines.length && !new RegExp(`^ {0,3}${marker}{3,}\\s*$`).test(lines[index])) {
        code.push(lines[index])
        index += 1
      }
      if (index < lines.length) index += 1
      blocks.push(
        <pre
          key={blockKey}
          className="my-2 max-w-full overflow-x-auto border border-border bg-muted p-2 font-mono text-[11px] leading-relaxed"
        >
          <code className={language === "" ? undefined : `language-${language}`}>
            {code.join("\n")}
          </code>
        </pre>,
      )
      blockIndex += 1
      continue
    }

    const heading = lines[index].match(/^ {0,3}(#{1,6})\s+(.+?)\s*#*\s*$/)
    if (heading !== null) {
      const level = Math.min(heading[1].length, 4)
      const Heading = `h${level}` as "h1" | "h2" | "h3" | "h4"
      const headingClass =
        level === 1
          ? "mt-2 text-base font-semibold"
          : level === 2
            ? "mt-2 text-sm font-semibold"
            : "mt-2 text-xs font-semibold uppercase tracking-[0.08em]"
      blocks.push(
        <Heading key={blockKey} className={headingClass}>
          {renderInline(heading[2], blockKey)}
        </Heading>,
      )
      index += 1
      blockIndex += 1
      continue
    }

    if (/^ {0,3}([-*_])(?:\s*\1){2,}\s*$/.test(lines[index])) {
      blocks.push(<hr key={blockKey} className="my-3 border-0 border-t border-border" />)
      index += 1
      blockIndex += 1
      continue
    }

    if (/^ {0,3}>\s?/.test(lines[index])) {
      const quoteLines: string[] = []
      while (index < lines.length && /^ {0,3}>\s?/.test(lines[index])) {
        quoteLines.push(lines[index].replace(/^ {0,3}>\s?/, ""))
        index += 1
      }
      blocks.push(
        <blockquote
          key={blockKey}
          className="my-2 border-l-2 border-brand-pink bg-accent px-3 py-2 text-muted-foreground"
        >
          {renderMarkdownBlocks(quoteLines.join("\n"), blockKey)}
        </blockquote>,
      )
      blockIndex += 1
      continue
    }

    if (
      index + 1 < lines.length &&
      lines[index].includes("|") &&
      isTableDivider(lines[index + 1])
    ) {
      const headers = tableCells(lines[index])
      index += 2
      const rows: string[][] = []
      while (index < lines.length && lines[index].trim() !== "" && lines[index].includes("|")) {
        rows.push(tableCells(lines[index]))
        index += 1
      }
      blocks.push(
        <div key={blockKey} className="my-2 max-w-full overflow-x-auto">
          <table className="w-full min-w-max border-collapse text-left text-[11px]">
            <thead>
              <tr>
                {headers.map((header, cellIndex) => (
                  <th
                    key={`${blockKey}-header-${cellIndex}`}
                    className="border border-border bg-muted px-2 py-1 font-semibold"
                  >
                    {renderInline(header, `${blockKey}-header-${cellIndex}`)}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {rows.map((row, rowIndex) => (
                <tr key={`${blockKey}-row-${rowIndex}`}>
                  {headers.map((_, cellIndex) => (
                    <td
                      key={`${blockKey}-cell-${rowIndex}-${cellIndex}`}
                      className="border border-border px-2 py-1 align-top"
                    >
                      {renderInline(
                        row[cellIndex] ?? "",
                        `${blockKey}-cell-${rowIndex}-${cellIndex}`,
                      )}
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>,
      )
      blockIndex += 1
      continue
    }

    const unordered = lines[index].match(/^ {0,3}([-+*])\s+(.+)$/)
    if (unordered !== null) {
      const items: string[] = []
      while (index < lines.length) {
        const item = lines[index].match(/^ {0,3}([-+*])\s+(.+)$/)
        if (item === null) break
        items.push(item[2])
        index += 1
      }
      blocks.push(
        <ul key={blockKey} className="my-2 list-disc space-y-1 pl-5">
          {items.map((item, itemIndex) => {
            const task = item.match(/^\[([ xX])\]\s+(.*)$/)
            return (
              <li key={`${blockKey}-item-${itemIndex}`}>
                {task !== null && (
                  <span
                    aria-hidden="true"
                    className={cn(
                      "mr-1 inline-flex size-3 items-center justify-center border text-[9px] leading-none",
                      task[1].toLowerCase() === "x"
                        ? "border-brand-pink bg-brand-pink text-foreground"
                        : "border-input",
                    )}
                  >
                    {task[1].toLowerCase() === "x" ? "✓" : ""}
                  </span>
                )}
                {renderInline(task?.[2] ?? item, `${blockKey}-item-${itemIndex}`)}
              </li>
            )
          })}
        </ul>,
      )
      blockIndex += 1
      continue
    }

    const ordered = lines[index].match(/^ {0,3}\d+[.)]\s+(.+)$/)
    if (ordered !== null) {
      const items: string[] = []
      while (index < lines.length) {
        const item = lines[index].match(/^ {0,3}\d+[.)]\s+(.+)$/)
        if (item === null) break
        items.push(item[1])
        index += 1
      }
      blocks.push(
        <ol key={blockKey} className="my-2 list-decimal space-y-1 pl-5">
          {items.map((item, itemIndex) => (
            <li key={`${blockKey}-item-${itemIndex}`}>
              {renderInline(item, `${blockKey}-item-${itemIndex}`)}
            </li>
          ))}
        </ol>,
      )
      blockIndex += 1
      continue
    }

    const paragraphLines: string[] = []
    while (
      index < lines.length &&
      lines[index].trim() !== "" &&
      !/^ {0,3}(?:#{1,6}\s|```|~~~|>|[-+*]\s+|\d+[.)]\s+)/.test(lines[index])
    ) {
      paragraphLines.push(lines[index])
      index += 1
    }
    blocks.push(
      <p key={blockKey} className="my-2 whitespace-pre-wrap break-words leading-relaxed">
        {renderInline(paragraphLines.join("\n"), blockKey)}
      </p>,
    )
    blockIndex += 1
  }

  return blocks
}

function MarkdownResponse({ text }: { text: string }) {
  return (
    <div className="font-mono text-xs leading-relaxed">
      {renderMarkdownBlocks(text, "markdown")}
    </div>
  )
}

export function ChatSidebar({
  open,
  selections,
  onUnpin,
  onClearSelections,
  onChart,
  format,
}: {
  open: boolean
  selections: Selection[]
  onUnpin: (selection: Selection) => void
  onClearSelections: () => void
  onChart: (update: ChartUpdate) => void
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
        charts: [],
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
          onChart: (update) => {
            onChart(update)
            patchAssistant(assistantId, (item) => ({
              ...item,
              charts: [...item.charts, update.label],
            }))
          },
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
          <Button
            type="button"
            onClick={onClearSelections}
            size="sm"
            className="border border-brand-pink px-3 text-[11px] font-semibold uppercase tracking-[0.12em]"
          >
            clear pins
          </Button>
        </div>
      )}

      {selections.length > 0 && (
        <div className="flex flex-wrap gap-1.5 border-b border-border px-3 py-2">
          {selections.map((selection) => (
            <SelectionChip
              key={selectionKey(selection)}
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
              answers from read-only SQL against the DuckDB data and can push the numbers straight
              onto the charts.
            </p>
            <div className="flex w-full max-w-[300px] flex-col gap-2">
              {STARTER_PROMPTS.map((prompt) => (
                <button
                  key={prompt}
                  type="button"
                  disabled={busy}
                  onClick={() => void send(prompt)}
                  className="border border-foreground bg-foreground px-3 py-2 text-left text-sm leading-relaxed text-background transition-opacity hover:opacity-80 disabled:pointer-events-none disabled:opacity-50"
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
                  <SqlChip key={`sql-${index}`} sql={sql} />
                ))}
                {item.charts.map((label, index) => (
                  <ChartChip key={`chart-${index}`} label={label} />
                ))}
                {item.text !== "" && (
                  <div className="break-words">
                    <MarkdownResponse text={item.text} />
                    {item.pending && <span className="animate-step-blink">▍</span>}
                  </div>
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
