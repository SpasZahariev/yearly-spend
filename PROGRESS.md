## Issue #2 - Scaffold workspace, DB schema, and empty pixel app

- Added the Rust workspace (`core`, `ingest`, `api`), DuckDB schema, seeded account/category metadata, FX cache, LLM provider config, and `spend` CLI scaffold.
- Added the Axum static frontend server and `GET /api/meta`, plus the Vite/React/Tailwind/shadcn pixel shell with design tokens.
- Fixed first-run database initialization: DuckDB sequences now generate primary keys, table discovery uses `schema_name`, and FX date parameters use explicit casts.
- Added regression coverage for generated IDs, metadata responses, schema initialization, and FX cache reads.
- Wired and verified cargo fmt/clippy/tests plus frontend lint/format/build. Created the `issue-2-scaffold` branch.

## Issue #4 - First real number: dashboard with spend from Neon

- Ingested the Neon corpus (2022-2026, 5 files) into `data/spend.duckdb`, with LLM backfill for uncategorized rows; set `LLM_MODEL` in `.env` from the local server's `/v1/models`.
- Added read-only year-scoped queries in `core`: `summary` (income, spend excl. `transfer_out`, moved, net), `monthly_spend` (always 12 months), `category_breakdown` (largest first, colors, percentages), each with unit tests.
- Added `GET /api/summary?year=`, `GET /api/series/monthly?year=`, `GET /api/categories?year=` with 400 on missing/invalid `year`, plus API integration tests against a temp DuckDB file with hand-checked values.
- Built the initial dashboard: KPI cards (income, spend), 12-month spend bar chart, color-coded category donut with legend, and a year picker populated from `/api/meta` that refetches on change; pixel tokens (2px borders, square corners, pixel font, dither band) applied via Recharts with animations disabled.
- Verified end-to-end in a browser: rendered values for 2022/2024/2025/2026 match direct SQL totals exactly; no console errors. Full `just check` green.

## Issue #3 - Ingest Neon CSVs idempotently

- Added semicolon-delimited Neon CSV parsing with quoted fields, UTF-8 BOM support, and CRLF-preserving multiline subjects.
- Added natural-key transaction hashing that excludes Neon categories, SHA-based whole-file skips, and sequence-backed idempotent upserts.
- Mapped Neon labels into the fixed taxonomy, batched uncategorized rows through the configured local LLM with taxonomy response validation, and recorded LLM audits.
- Marked negative Neon outflows to Revolut, Swisscard AECS, and Interactive Brokers as `transfer_out`.
- Added parser, taxonomy, transfer, LLM validation, and idempotency regression coverage.
