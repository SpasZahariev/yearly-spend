# yearly-spend

Personal finance dashboard that turns raw bank exports (Neon, Revolut, Swisscard) into a clean, auditable spending overview. It ingests semicolon/comma CSVs, normalizes every amount to CHF via cached Frankfurter monthly FX rates, auto-categorizes transactions into a fixed 18-category taxonomy with an LLM, and serves a pixel-styled, fully local dashboard with year/month views, multi-currency display, an interactive Sankey money-flow, and a chat inspector that can query the DuckDB file with read-only SQL and push results back onto the charts.

![demo](images/demo.gif)
*Demo GIF — dashboard, AI chat, and inline transaction editing (1920x1080). I will replace this placeholder with a recorded walkthrough.*

## Screenshots

All captures are 1920×1080 viewport, full-page (`images/`).

| Dashboard — yearly overview | Month view |
|---|---|
| ![dashboard yearly](images/dashboard.png) | ![dashboard month](images/dashboard-month.png) |
| KPI cards, money-flow Sankey, yearly totals + cumulative, category donut. Year picker, currency (CHF/USD/EUR) and month/year view in the navbar. | Same data scoped to a single month — monthly bar chart, daily drill-down, and month-scoped Sankey/categories. |

| Transactions — inline overrides | AI chat + pins |
|---|---|
| ![transactions](images/transactions.png) | ![chat](images/chat.png) |
| Filterable, paginated table. Category is a taxonomy dropdown, `is_transfer` is a checkbox. `PATCH /api/transactions/{id}` flips `kind` via `kind_for_transfer` and persists on a short-lived read-write connection; lists use a read-only connection. | Click any bar/slice/point to pin a selection chip (chart, value, year/month, note). The inspector sends `{message, selections, history}` to `POST /api/chat`. The model replies over SSE with `run_sql` traces and `render_dashboard` overlays. |

| AI-generated data mode |
|---|
| ![ai mode](images/dashboard-ai.png) |
| When the assistant pushes data, affected cards get a super-thin pink border (`!border-brand-pink`) and a small pink bot icon (top-right) — e.g. top-3 categories + Sankey filtered to 3 outflows, KPIs in USD, or month-scoped data. Changing any navbar control (currency, year, month, view) clears the overlay and returns to live DB data. |

## Tech stack

| Layer | Tech |
|---|---|
| **Backend** | Rust (edition 2024) workspace — `core` (schema/migrations, DuckDB, FX cache, LLM client, `run_sql`/`render_dashboard` tool loop), `ingest` (Neon/Revolut/Cashback parsers, SHA-based file skip + natural-key upserts, 60-row LLM batch categorization), `api` (Axum on `127.0.0.1:3000`, SSE `POST /api/chat`, JSON `GET /api/meta,/summary,/series/*,/categories,/transactions`, `PATCH /api/transactions/{id}`, `GET /api/fx`, SPA fallback) |
| **Database** | DuckDB (bundled) — single file `data/spend.duckdb` (`DB_PATH`), migrations in `core/src/schema.rs`, 18-category taxonomy + 4 seeded accounts |
| **Frontend** | Vite + React 19 + TypeScript + Tailwind 4 + shadcn/ui + Recharts (`pnpm`), hash routing (`#/` dashboard, `#/transactions`), `app-frame` 2px pixel design, currency formatting with live Frankfurter spot rates |
| **LLM / FX** | Local `llama-server` (OpenAI-compatible `/v1/chat/completions` SSE) or Gemini (`streamGenerateContent` SSE) — selected via `.env`; Frankfurter for FX monthly averages + spot (cached in `fx_rates`) |
| **Tooling** | `just` task runner, `cargo fmt/clippy/test` (`-D warnings`, `unsafe_code = "forbid"`), `pnpm lint/format/build`, `cargo` workspace, `reqwest` + `sqlparser` (DuckDB dialect + lexical WITH-tail guard), `tower-http` |

## Highlights 

* **Full-stack ownership** — Rust API serving a Vite SPA from the same binary, shared `spend-core` crate, end-to-end type safety from DuckDB to Recharts.
* **LLM as data tool, not narrator** — `run_sql` is SELECT-only (sqlparser + CTE-tail guard, read-only connection, 100 rows / 8 rounds), every ingest batch is audited in `llm_calls`, taxonomy is validated; `render_dashboard` is JSON-schema validated and merges over live API data per year.
* **Conversational memory** — frontend sends the last 20 turns (`{role, content}`) with every `ask`; backend replays `history` before the new user message for both Local (OpenAI `messages`) and Gemini (`contents`) so follow-ups (“what about last year?”) work without server state.
* **Security & correctness** — `unsafe_code = "forbid"`, read-only connections by default, short-lived read-write only for `PATCH`, FX and ingest are idempotent (file SHA + `UNIQUE(source, source_key)`).

## Setup

### Requirements

* Rust 1.85+ (edition 2024)
* `pnpm` 9+
* `just` (optional — `cargo`/`pnpm` commands work directly)
* DuckDB builds with the bundled feature — no external DB needed

### Quick start

```sh
cp .env.example .env
just setup          # installs deps, builds Rust + frontend
just serve          # builds frontend, starts http://127.0.0.1:3000
# or without just:
cargo build
pnpm --dir frontend install --frozen-lockfile
pnpm --dir frontend build
cargo run -p spend-api
```

Open http://127.0.0.1:3000. The dashboard works without an LLM — it shows whatever is already in `data/spend.duckdb` (empty on first checkout).

### Ingest statements

```sh
cargo run -p ingest -- ingest statements/
# or: just ingest
```

Place exports under `statements/<source>/` — source is inferred from the nearest ancestor directory named `neon`, `revolut`, or `cashback_cards` (case-insensitive). Already-ingested files are skipped by SHA; rows are upserted by natural key.

### LLM setup

The same `.env` drives both ingest categorization and chat. You only need an LLM for those two features.

#### Option A — local model (default, fully offline)

The repo defaults to an OpenAI-compatible local server:

```sh
# .env
LLM_PROVIDER=local
LLM_BASE_URL=http://localhost:11434/v1
LLM_API_KEY=sk-local-token
LLM_MODEL=/path/to/model.gguf   # or model id from /v1/models
# example: unsloth/Qwen3-8B-GGUF or any OpenAI-compatible endpoint
DB_PATH=data/spend.duckdb
```

Run a server that speaks `/v1/chat/completions` with SSE, e.g.:

```sh
# llama.cpp llama-server
llama-server --model /path/to/model.gguf --port 11434 --host 127.0.0.1
# or Ollama (OpenAI compat)
ollama serve
ollama run qwen3:8b
```

Then ingest/chat will use it. No key is sent beyond `sk-local-token`.

#### Option B — provider API key (Gemini)

```sh
# .env
LLM_PROVIDER=gemini
LLM_BASE_URL=http://localhost:11434/v1   # unused for gemini, keep default
LLM_API_KEY=sk-local-token
LLM_MODEL=                                # unused for gemini
GEMINI_API_KEY=your_gemini_api_key
GEMINI_MODEL=gemini-2.5-flash
DB_PATH=data/spend.duckdb
```

Only `GEMINI_API_KEY` is required to switch; the code picks `gemini` when the key is present and maps `history` to Gemini `contents` (`user`/`model` roles) + `system_instruction`. Keep `.env` gitignored and copy from `.env.example`.

> **No LLM?** Leave `.env` as-is. Ingest will leave rows uncategorized, but `cargo run -p spend-api` and the dashboard still work. You can categorize later after configuring a provider and re-running ingest.

## Development

```sh
just check          # fmt --check, clippy -D warnings, cargo test, pnpm lint/format:check/build
just frontend-dev   # Vite dev server
just rust-test      # cargo test --workspace
```

### Verification

* Pick years/months in the navbar and cross-check KPI/bar/donut numbers against `SELECT sum(-amount_chf) FILTER (WHERE kind='spend')` on `data/spend.duckdb`.
* Try the chat: pin a bar/slice, ask “top 3 categories in 2026?”, confirm the pink-bordered cards + bot icon appear and the Sankey shows exactly 3 outflows. Change currency/year to confirm the overlay clears.
* Transactions: change a row’s category or transfer checkbox and verify the dashboard refetches on hash-nav back to `#/`.

## Project layout

```
core/       spend-core crate
ingest/     spend CLI
api/        Axum server, serves frontend/dist
frontend/   Vite + React 19 + Tailwind 4 + Recharts
images/     screenshots (1920x1080) + demo.gif placeholder
data/       spend.duckdb (gitignored)
statements/ CSV exports per source
```

License: MIT
