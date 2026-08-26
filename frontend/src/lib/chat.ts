import type { ChartUpdate, Selection } from "@/types"

/**
 * Streams one chat request over SSE. `POST /api/chat` answers with a text
 * event stream: bare `data:` lines are reply tokens, `event: tool` lines
 * carry `{"sql": ...}` (the read-only tool the model invoked), `event: chart`
 * lines carry a dashboard update (the `render_dashboard` tool the model
 * invoked), and `event: error` lines terminate with an error message.
 */
export interface ChatCallbacks {
  onToken: (text: string) => void
  onTool: (sql: string) => void
  onChart: (update: ChartUpdate) => void
  onError: (message: string) => void
}

export async function streamChat(
  message: string,
  selections: Selection[],
  callbacks: ChatCallbacks,
  signal: AbortSignal,
): Promise<void> {
  const res = await fetch("/api/chat", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ message, selections }),
    signal,
  })
  if (!res.ok || res.body === null) {
    const detail = await res.text().catch(() => "")
    throw new Error(detail.trim() || `/api/chat responded ${res.status}`)
  }
  const reader = res.body.getReader()
  const decoder = new TextDecoder()
  let buffer = ""
  for (;;) {
    const { done, value } = await reader.read()
    if (done) break
    buffer += decoder.decode(value, { stream: true })
    // SSE frames are separated by a blank line.
    let separator: number
    while ((separator = buffer.indexOf("\n\n")) !== -1) {
      const frame = buffer.slice(0, separator)
      buffer = buffer.slice(separator + 2)
      let eventType = "message"
      const dataLines: string[] = []
      for (const line of frame.split("\n")) {
        if (line.startsWith("event:")) {
          eventType = line.slice("event:".length).trim()
        } else if (line.startsWith("data:")) {
          // SSE strips exactly one optional space after the colon; any
          // further leading whitespace is part of the token.
          const raw = line.slice("data:".length)
          dataLines.push(raw.startsWith(" ") ? raw.slice(1) : raw)
        }
      }
      if (dataLines.length === 0) continue
      // Multi-line SSE data is re-joined with newlines.
      const data = dataLines.join("\n")
      if (eventType === "tool") {
        let sql = data
        try {
          sql = JSON.parse(data).sql ?? data
        } catch {
          // fall back to the raw payload
        }
        callbacks.onTool(sql)
      } else if (eventType === "chart") {
        try {
          callbacks.onChart(JSON.parse(data) as ChartUpdate)
        } catch {
          // A malformed payload is dropped; the reply text still streams.
        }
      } else if (eventType === "error") {
        callbacks.onError(data)
      } else {
        callbacks.onToken(data)
      }
    }
  }
}
