/**
 * Lightweight DuckDB SELECT formatter for the chat sidebar.
 * No dependencies. Handles the queries the assistant actually emits:
 *  - single SELECT with optional JOINs
 *  - WHERE with AND/OR/BETWEEN
 *  - GROUP BY / ORDER BY / LIMIT / HAVING
 *  - aggregate functions like SUM(), COUNT()
 *
 * It normalises whitespace, puts major clauses on new lines, indents
 * continuation lines, and uppercases canonical keywords for readability.
 */

const JOIN_CLAUSES = ["LEFT JOIN", "RIGHT JOIN", "INNER JOIN", "OUTER JOIN", "JOIN"] as const
const MAJOR_CLAUSES = [
  "FROM",
  "WHERE",
  "GROUP BY",
  "ORDER BY",
  "LIMIT",
  "HAVING",
  "WINDOW",
  "UNION",
] as const

export function formatSql(sql: string): string {
  let s = sql.trim()

  // Collapse interior whitespace to single spaces, but keep a single space
  // between tokens. String literals keep their content; the collapse is safe
  // because SQL strings are single-quoted and contain no extra whitespace that
  // matters beyond one space.
  s = s.replace(/\s+/g, " ")

  // Put JOINs on new lines first, then major clauses.
  for (const j of JOIN_CLAUSES) {
    const re = new RegExp(`\\s+${escapeReg(j)}\\s+`, "gi")
    s = s.replace(re, `\n${j} `)
  }
  for (const c of MAJOR_CLAUSES) {
    const re = new RegExp(`\\s+${escapeReg(c)}\\s+`, "gi")
    s = s.replace(re, `\n${c} `)
  }

  // SELECT -> SELECT\n  <fields>
  s = s.replace(/^SELECT\s+/i, "SELECT\n  ")

  // Within the SELECT field list (before the first FROM), put each comma
  // separated expression on its own indented line. Handles SUM(), COUNT(), etc
  // where commas appear inside parentheses - we only split at top-level commas.
  const fromIdx = s.indexOf("\nFROM ")
  if (fromIdx !== -1) {
    const selectPart = s.slice(0, fromIdx)
    const rest = s.slice(fromIdx)
    s = splitSelectFields(selectPart) + rest
  } else {
    // No FROM (e.g. SELECT 1) - still split select list
    if (s.startsWith("SELECT\n  ")) {
      s = splitSelectFields(s)
    }
  }

  // WHERE continuation: each AND / OR on a new indented line
  s = s.replace(/\s+AND\s+/gi, "\n  AND ")
  s = s.replace(/\s+OR\s+/gi, "\n  OR ")

  // Uppercase canonical keywords (preserve identifiers and literal case otherwise)
  s = s.replace(
    /\b(select|from|where|group\s+by|order\s+by|limit|having|window|union|join|left\s+join|right\s+join|inner\s+join|outer\s+join|on|and|or|between|as|asc|desc|by)\b/gi,
    (m) => m.toUpperCase(),
  )

  // Clean trailing spaces per line, keep intentional indentation
  s = s
    .split("\n")
    .map((line) => line.trimEnd())
    .join("\n")

  return s
}

function escapeReg(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")
}

function splitSelectFields(selectPart: string): string {
  // selectPart starts with "SELECT\n  " followed by fields.
  // Split at top-level commas (depth 0, not inside parentheses).
  const prefix = "SELECT\n  "
  if (!selectPart.startsWith(prefix)) return selectPart
  const fieldsRaw = selectPart.slice(prefix.length)
  const fields: string[] = []
  let depth = 0
  let current = ""
  let inSingleQuote = false
  for (let i = 0; i < fieldsRaw.length; i++) {
    const ch = fieldsRaw[i]
    if (ch === "'" && fieldsRaw[i - 1] !== "\\") {
      // Handle '' escaped quotes: stay inside string
      if (inSingleQuote && fieldsRaw[i + 1] === "'") {
        current += "''"
        i++
        continue
      }
      inSingleQuote = !inSingleQuote
      current += ch
      continue
    }
    if (!inSingleQuote) {
      if (ch === "(") depth++
      else if (ch === ")") depth = Math.max(0, depth - 1)
      else if (ch === "," && depth === 0) {
        fields.push(current.trim())
        current = ""
        continue
      }
    }
    current += ch
  }
  if (current.trim() !== "") fields.push(current.trim())
  if (fields.length <= 1) return selectPart
  return prefix + fields.join(",\n  ")
}

// Simple keyword highlighter: wraps canonical keywords in a styled span.
// Used by the SqlChip component. Keeps string literals intact.
const HIGHLIGHT_RE =
  /\b(SELECT|FROM|WHERE|GROUP BY|ORDER BY|LIMIT|HAVING|WINDOW|UNION|JOIN|LEFT JOIN|RIGHT JOIN|INNER JOIN|OUTER JOIN|ON|AND|OR|BETWEEN|AS|ASC|DESC|COUNT|SUM|AVG|MAX|MIN|DATE)\b/gi

export function tokenizeSqlForHighlight(sql: string): { text: string; isKeyword: boolean }[] {
  const tokens: { text: string; isKeyword: boolean }[] = []
  let last = 0
  let m: RegExpExecArray | null
  // Reset regex state
  HIGHLIGHT_RE.lastIndex = 0
  while ((m = HIGHLIGHT_RE.exec(sql)) !== null) {
    if (m.index > last) {
      tokens.push({ text: sql.slice(last, m.index), isKeyword: false })
    }
    tokens.push({ text: m[0].toUpperCase(), isKeyword: true })
    last = m.index + m[0].length
  }
  if (last < sql.length) tokens.push({ text: sql.slice(last), isKeyword: false })
  return tokens
}
