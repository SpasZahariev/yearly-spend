# yearly-spend

Personal yearly spending dashboard. Ingests bank statement CSVs (Neon, Revolut), normalizes everything to CHF (Frankfurter monthly FX rates), auto-categorizes transactions via an LLM into a fixed taxonomy, stores them in DuckDB, and serves a pixel-styled dashboard with income/spend KPIs, a 12-month spend chart, and a category breakdown.

## Stack

- `core/` - Rust crate: DuckDB schema, config, FX cache, LLM client, read-only year-scoped queries
- `ingest/` - `spend` CLI: parses and idempotently upserts statement CSVs
- `api/` - Axum server: JSON API + serves the built frontend at `http://127.0.0.1:3000`
- `frontend/` - Vite + React + Tailwind + shadcn + Recharts
- `data/spend.duckdb` - the database (created on first run)

## Requirements

- Rust 1.85+ (edition 2024)
- pnpm
- `just` (optional, for the recipe shortcuts below)
- An OpenAI-compatible LLM endpoint for ingest-time categorization (local `llama-server` by default, or Gemini via `.env`) - not needed just to run the dashboard

## Setup

```sh
cp .env.example .env
cargo build
pnpm --dir frontend install --frozen-lockfile
pnpm --dir frontend build
```

Edit `.env` if you use a non-default LLM provider (see `.env.example`).

## Run

```sh
cargo run -p spend-api
```

Open http://127.0.0.1:3000.

## Ingest statements

Place CSV exports under `statements/` (one subdirectory per source, e.g. `statements/Neon/`) and run:

```sh
cargo run -p ingest -- ingest statements/
```

Ingest is idempotent: already-ingested files are skipped by file hash. Non-CHF amounts are converted with one Frankfurter call per currency-month (cached in the DB). Uncategorized rows are batched through the configured LLM.

## Development

```sh
just setup          # .env + deps + full build
just serve          # build frontend, start API
just check          # fmt, clippy, tests, lint, format-check, build
just frontend-dev   # Vite dev server for frontend-only work
just ingest         # ingest statements/
```
