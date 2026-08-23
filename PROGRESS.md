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

## Issue #5 - Ingest Revolut CSVs with FX conversion

- Added comma-CSV Revolut parsing (unquoted) with COMPLETED-only ingestion: `REVERTED` rows dropped, exact-duplicate rows collapsed by a natural key over started/type/description/amount/currency, and fee netted into the amount so `balance` reconciles.
- Applied the hygiene rules: `Exchange` FX-swap pairs, zero-amount `Closing transaction` rows, and `TEMP_BLOCK` rows are `internal` (never spend); `Topup` rows are `transfer_in`; other rows map to `spend`/`income` by sign and are LLM-categorized.
- Converted non-CHF rows to CHF at the frankfurter.dev monthly average, one call per currency-month, cached in `fx_rates`; filtered out the anchor day Frankfurter prepends so averages cover only the requested month.
- Extracted shared LLM categorization into `ingest/src/categorize.rs` (batching, taxonomy validation, audit) reused by both Neon and Revolut; moved `IngestReport` to `ingest::main`.
- Ingested the corpus: 861 unique rows (122/231/320/188 across 2023-2026), 67 `fx_rates` rows (one per non-CHF currency-month), 0 hygiene rows in spend, 16 LLM calls with 0 failures.
- Verified offline idempotency (re-ingest skips all files with network endpoints pointed at dead ports), and that the dashboard renders 2022-2026 with zero frontend changes and no console errors.

## Issue #6 - Ingest cashback card CSV statements programmatically

- Added a deterministic Swisscard AECS cashback parser (`ingest/src/cashback.rs`): fully-quoted 12-column CSV, `dd.mm.yyyy` dates, sign flip so debits are negative spend. No LLM in extraction. FX rows (EUR/GBP/USD) keep the foreign amount as `amount_orig` and the card's CHF charge as `amount_chf`, sanity-checked against the `fx_rates` cache at a 10% tolerance (card markup observed within +1.9% to +4.9%).
- Used the export's `Merchant Category` as source of truth, mapped into the fixed taxonomy (Travel/Groceries/Food and Drink/Shopping/Entertainment/Health and Beauty/Auto/Finance). `CASHBACK` credit lines land as income; `YOUR PAYMENT (DD)` lines (category `Payment`) land as `transfer_in` funding legs excluded from spend. Labels with no taxonomy counterpart (General/Services/Family and Household) get the shared taxonomy-constrained LLM backfill.
- Natural key = hash(card, date, normalized description, signed CHF cents) so a re-downloaded month under a new filename upserts in place; whole-file SHA skip before parsing. Each `transfer_in` is cross-validated against the paired Neon `Swisscard AECS` row (exact date + opposite amount + matching statement month); a malformed row or a validation mismatch hard-fails inside the transaction with no partial writes.
- Ingested the corpus: 23 files, 746 raw rows -> 729 unique (17 byte-identical within-file export duplicates collapsed by the natural key), 0 null categories, 19 LLM batch calls with 0 failures. Kind split: 704 spend, 23 transfer_in, 2 income.
- Verified end-to-end: re-ingest is a no-op (all 23 files skip before parsing); dashboard income/spend/monthly/category values for 2024/2025/2026 match direct SQL exactly; no console errors. Full `just check` green.
