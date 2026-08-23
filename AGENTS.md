# AGENTS.md

Guidance for agents working in this repository.

## What this project is

yearly-spend is a personal yearly spending dashboard. It ingests bank statement
CSVs (Neon, Revolut), normalizes all amounts to CHF using monthly FX rates,
auto-categorizes transactions via an LLM into a fixed taxonomy, stores
everything in a DuckDB file, and serves a pixel-styled dashboard (income/spend
KPIs, 12-month spend chart, category donut, year picker).

## Tech stack and layout

- Rust workspace (edition 2024): `core`, `ingest`, `api`
- Frontend: Vite + React 19 + TypeScript + Tailwind 4 + shadcn/ui + Recharts, managed with **pnpm**
- Database: DuckDB (bundled), single file at `data/spend.duckdb` (path from `DB_PATH`)
- Task runner: `just` (see justfile); frontend work uses pnpm scripts

```
core/        spend-core crate, shared by ingest and api
  src/schema.rs    DDL + seeds (accounts, 18-category taxonomy) + migrations
  src/db.rs        ingest_connection (read-write, migrates) / api_connection
                   (read-only) / api_write_connection (short-lived read-write
                   for inline overrides, drops + releases the lock)
  src/config.rs    .env loading, LLM provider selection
  src/fx.rs        Frankfurter client: cached_rate (fx_rates table) + monthly average
  src/llm.rs       OpenAI-compatible chat client (local llama-server or Gemini)
  src/chat.rs      Chat tool loop: single run_sql tool (SELECT-only via
                   sqlparser DuckDB dialect + lexical CTE-tail guard, read-only
                   connection, 100-row / 8-round caps), local + Gemini SSE
                   streaming, pinned selection context in the system prompt
  src/queries.rs   Read-only year-scoped queries: summary, monthly_spend,
                   category_breakdown, meta, list_transactions/get_transaction;
                   set_transaction + kind_for_transfer for inline overrides
ingest/      `spend` CLI (cargo run -p ingest -- ingest <paths>)
  src/detect.rs    Source detection from ancestor dir name, recursive CSV collection
  src/neon.rs      Semicolon CSV parser (BOM, quoted fields, multiline subjects)
  src/revolut.rs   Comma CSV parser (COMPLETED-only, fee netting, duplicate collapse)
  src/categorize.rs  Shared LLM categorization: 60-row batches, taxonomy validation,
                   llm_calls audit rows
api/         Axum server on 127.0.0.1:3000
  src/main.rs      /api/meta, /api/summary, /api/series/*, /api/categories
                   (all take ?year=), /api/transactions (GET list + PATCH
                   override), /api/chat (POST, SSE); serves frontend/dist as an
                   SPA fallback
frontend/    Vite app; src/App.tsx is the dashboard (KPI cards, Recharts bar +
             donut, year picker, chart-click pinning); src/components/
             TransactionsTable.tsx = inline-override table; src/components/
             ChatSidebar.tsx = inspector chat (chips, streaming items);
             src/lib/chat.ts = fetch+ReadableStream SSE client; src/components/ui
             = shadcn primitives
statements/  CSV exports, one subdirectory per source (Neon/, Revolut/,
             cashback_cards/); source is detected from that directory name
data/        spend.duckdb lives here (gitignored, created on first run)
```

## Terminology

- **kind**: the 5 transaction kinds - `spend`, `income`, `transfer_out`,
  `transfer_in`, `internal`. Spend aggregates exclude `transfer_out`; `internal`
  rows never count as spend.
- **taxonomy**: the fixed 18 categories seeded in `schema.rs` (with colors).
  LLM categorization may only return taxonomy names.
- **natural key** / `source_key`: stable per-transaction hash (source-specific
  columns) making `UNIQUE (source, source_key)` upserts idempotent.
- **LLM backfill / categorization**: uncategorized rows batched (60 per call)
  to the LLM; every call is audited in the `llm_calls` table.
- **FX**: non-CHF amounts converted at the Frankfurter *monthly average* rate,
  one call per currency-month, cached in `fx_rates`. The anchor day Frankfurter
  prepends is filtered out so the average covers only the requested month.
- **hygiene rules** (Revolut): `Exchange` FX-swap pairs, zero-amount `Closing
  transaction` rows, and `TEMP_BLOCK` rows are `internal`; `Topup` rows are
  `transfer_in`.
- **idempotency**: two layers - whole-file skip by SHA in `ingested_files`
  (skipped before parsing), plus natural-key upserts for rows.
- **pixel app**: the frontend design system - 2px borders, square corners,
  pixel font, dither band; Recharts animations disabled.
- **just**: the justfile task runner (`just setup`, `just serve`, `just check`).

## How the important components work

- **Ingest pipeline**: `detect::collect_csvs` walks the given paths recursively;
  each file's source is detected from the nearest ancestor directory named
  `neon` | `revolut` | `cashback_cards` (case-insensitive). Parsers produce
  rows; file SHA is checked first so re-ingests skip whole files offline;
  rows are upserted by natural key; rows without a category go through
  `categorize.rs` in batches and must map back to a valid taxonomy entry
  (invalid batches fail the run).
- **DB access split**: the `ingest` process owns read-write + migrations. The
  API opens the same file read-only per request (bootstrapping the schema once
  on a fresh checkout) and runs reads on blocking threads. The single exception
  is `PATCH /api/transactions/{id}`, which opens a short-lived read-write
  connection (`api_write_connection`), writes, and drops it before returning so
  the file lock is released and `spend ingest` can still run while the API is
  idle.
- **Dashboard data flow**: frontend fetches `/api/meta` (accounts, categories,
  available year periods) plus `/api/summary`, `/api/series/monthly`,
  `/api/categories` with `?year=`; the year picker refetches on change.
  The transactions table fetches `/api/transactions?year=...` (optional
  source/category/month filters + paging) and edits rows via
  `PATCH /api/transactions/{id}` (category and/or `is_transfer`), refetching
  the KPIs/charts after each override.
  Missing/invalid `year` -> 400.
- **Chat**: clicking any chart element pins a selection chip (chart, series,
  label, value, year) in the right-side inspector; chips live in per-tab React
  state and are never persisted (reload -> empty sidebar). `POST /api/chat`
  takes `{message, selections}` and answers `text/event-stream`: bare `data:`
  frames are reply tokens, `event: tool` carries `{"sql": ...}`, `event: error`
  terminates with a message. The model's only tool is `run_sql`; SQL must parse
  as a single SELECT (sqlparser DuckDB dialect + lexical check that a WITH
  tail is SELECT/VALUES, since sqlparser parses `WITH ... INSERT` as a query),
  and runs on a short-lived read-only DuckDB connection. The system prompt
  documents the schema and tells the model the dashboard's spend formula
  (`sum(-amount_chf) FILTER (WHERE kind = 'spend')`; refund rows stored
  positive net against spend) so its totals match the rendered numbers.
  Handler caps: 20k-char messages, 10 selections, 180s timeout.

## End-to-end testing

1. `just setup` (creates `.env`, installs deps, builds everything)
2. `just serve` - builds the frontend and starts the API at http://127.0.0.1:3000
3. Drive the UI in a real browser (playwright or chrome-devtools tools):
   pick years with the year picker and inspect KPI cards, bar chart, donut.
4. Cross-check rendered values against direct SQL on `data/spend.duckdb`
   (totals per year, per month, per category). They must match exactly.
5. Check the browser console for errors; run `just check` and keep it green.

## Project conventions

- Package manager: **pnpm** for the frontend (`pnpm --dir frontend ...`), cargo for Rust.
- Rust tests are std `#[cfg(test)]` modules colocated in the file under test.
  The frontend has no test framework; its gate is lint + format + build.
- Only write tests for critical sections (parsers, idempotency, FX math,
  query math, LLM response validation). Do not pad the codebase with tests for
  code the compiler/type checker already proves - trust the compiler.
- TDD is expected for bug fixes: reproduce first (ideally end-to-end), then fix.
- `just check` (fmt, clippy with `-D warnings`, tests, lint, format, build) must
  pass before work is considered done.
- `unsafe_code = "forbid"` workspace-wide; no exceptions.
- Never commit secrets; `.env` is gitignored, use `.env.example` as the template.
- Do not auto-add AI co-author lines to commits.
