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

## Issue #7 - Month/year views with daily and cumulative detail

- Added read-only series queries in `core`: `yearly_spend` (per-year spend for every year with data, oldest first), `cumulative_spend` (running total within a year, all 12 months present), `daily_spend` (every day of the month present, leap-year aware); each with unit tests.
- Extended `summary` and `category_breakdown` with an optional `month` so KPIs and the donut can follow a single month; the API now serves `GET /api/series/yearly`, `GET /api/series/cumulative?year=`, `GET /api/series/daily?year=&month=`, and `/api/summary` + `/api/categories` accept an optional `month` (1-12 enforced, invalid values are 400s). API integration tests cover the new endpoints, month scoping, and 400 paths against a temp DuckDB file with hand-checked values.
- Built the header period controls: a month | year segmented view toggle, the year picker, and a month picker (month view only, defaults to the last month with data and resets when the year changes); the monthly chart card gained a month | day granularity sub-toggle.
- Yearly view renders per-year total bars with the selected year highlighted plus a step-quantized cumulative-spend line for the selected year; monthly view renders the 12 monthly bars with the selected month highlighted, and the day granularity renders daily bars of the selected month.
- KPI cards and the category donut refetch for the selected period (`?year=&month=` in month view, `?year=` in year view) with period labels (e.g. `CHF · Mar 2025`); loading state derived from the pending period key, no extra render passes.
- Verified end-to-end in a browser: rendered values for 2025/2026 (yearly, cumulative, monthly, daily, summary, categories) match direct SQL exactly; view toggle, month picker, and granularity sub-toggle switch charts as specified with the selected month highlighted; zero console errors. Full `just check` green.

## Issue #8 - Currency toggle (CHF/USD/EUR)

- Added `Fx::spot_rate` in `core`: one frankfurter.dev `/v1/latest?base=CHF&symbols=<to>` call returning `(rate, date)`; CHF is identity (no upstream call). Unit-tested rate extraction.
- Added `GET /api/fx?to=` to the API: USD/EUR only (anything else is 400), returns `{from, to, rate, date}`, 500 on upstream failure. Integration tests against a counting mock frankfurter server: exactly one upstream call per hit, rejection paths make none.
- Frontend: CHF/USD/EUR segmented toggle in the header. Every displayed number (KPI cards, all four charts, axis ticks, tooltips, donut legend) converts client-side as `value * rate` and formats as `USD 74'401.91` (Intl `de-CH`, `currencyDisplay: code`); CHF is the identity, so returning to CHF restores the API values exactly. KPI cards now render only once data is ready instead of flashing `0.00`.
- One `/api/fx` call per currency per session: rates cached in state with in-flight request dedupe, so re-renders (year/month toggles) and re-toggles make no FX calls; on FX failure the toggle reverts to the last good currency and the error banner is set.
- Verified end-to-end in a browser: CHF -> USD (1.2508) -> EUR (1.0692) -> CHF cycle converts and restores every visible value exactly (e.g. income `CHF 59'483.46` = SQL, `USD 74'401.91`, `EUR 63'599.72`); year switching while in USD made zero FX calls; returning to CHF made none; zero console errors. Full `just check` green.

## Issue #9 - Pair funding transfers across accounts

- Added `ingest/src/pair.rs`, run as a final ingest phase and via a new `spend pair` subcommand (`just pair`) so re-ingests that skip every file still pair. Pairing is a whole-DB pass over ungrouped `transfer_out`/`transfer_in` legs.
- Deterministic pairing per scope (Neon -> Revolut: out description contains `revolut`; Neon -> Swisscard: `swisscard` out against `cashback` `YOUR PAYMENT` ins). Pass A links exact same-date + exact CHF amount legs; pass B links the survivors when the inflow settles 1-3 days late (exact amount, inflow on or after the outflow, smallest delay preferred). Both passes are one-to-one and consume bucket members in id order, so the result is deterministic and stable across re-runs.
- Legs still unpaired after the deterministic passes go to the LLM in 60-row batches. Proposals are strictly validated before persisting: exact amount in cents, `in.dt >= out.dt`, gap <= 3 days, matching scope, one-to-one; an invalid response hard-fails the run. Legs sent to the LLM are recorded in a new `transfer_review` table (PK only, no FK - DuckDB forbids updating a referenced table) so re-runs do not re-review them, while deterministic pairing still runs on reviewed legs when a later statement arrives.
- Each pair becomes a `transfer_groups` row (out-leg amount, both accounts, out date) with both legs linked; `summary.moved` is now the sum of paired groups' out-leg amounts (unpaired transfer-flagged legs count as nothing but stay excluded from spend on both legs). Interactive Brokers outflows have no inflow in the corpus: they stay transfer-flagged, ungrouped, and out of both passes.
- Frontend: third KPI card (`moved`) in the dashboard summary, same pixel style as income/spend.
- Verified end-to-end on the real corpus: 53 groups (52 same-day + 1 settlement-late, 2024-07-16/17), every group has exactly 2 legs, all near-miss traps (in-before-out, close-but-different amounts, third-party gifts) stay unpaired with zero false pairs; moved per year matches SQL exactly (2023 1'605.00, 2024 6'761.95, 2025 20'772.40, 2026 7'290.40); re-pairing is a no-op; dashboard KPI cards match SQL for 2024/2026 with zero console errors. Full `just check` green.

## Issue #10 - Transactions table with inline overrides

- Added read + write queries in `core`: `Transaction`/`TransactionFilters`, `list_transactions` (newest first, filterable by year/month/source/category with `"uncategorized"` matching `category_id IS NULL`, paginated with a filtered total), `get_transaction`, `category_id_for_name`, and `set_transaction` (tri-state category: keep / set to NULL / set to id). The transfer flag maps onto the existing `kind` column via `kind_for_transfer` (negative -> `transfer_out`/`spend`, positive -> `transfer_in`/`income`), so no new schema column is needed and spend aggregates keep excluding transfers. Unit-tested all of it against an in-memory DB.
- Added `api_write_connection`: a short-lived read-write connection used only by the PATCH handler. Reads stay read-only per request; the write connection drops (and releases the DuckDB file lock) before the handler returns, so `spend ingest` can still open the database while the API is idle. Schema is ensured idempotently so a PATCH on a fresh checkout cannot fail.
- API: `GET /api/transactions` (optional `year`/`month`/`source`/`category`/`page`/`page_size`; returns `items` + `total`/`page`/`page_size`/`pages`; 400 on bad month/category/page) and `PATCH /api/transactions/{id}` (body `{category?, is_transfer?}`, at least one required; `category` a taxonomy name or `"uncategorized"` -> NULL; 404 unknown id, 400 unknown category/empty body; returns the updated row). API integration tests cover filtering, paging, validation, a category shift moving spend between donut slices, a transfer toggle removing a row from the spend KPI, and the 404/400 paths.
- Frontend: `TransactionsTable` component (below the charts) with category color chips, an inline category `<select>`, a transfer checkbox, source/category/month filters, and prev/next pagination. A successful override optimistically updates the row and calls back so the dashboard refetches its KPI cards and donut. Shared `getJson`/`patchJson` helpers in `lib/api` and shared types in `types.ts`; the header year/month selects gained `name` attributes.
- Verified end-to-end in a real browser on the corpus: category override moved a row from dining to food (donut `dining` 1'158.57 -> 1'149.21, `food` 0 -> 9.36, exact) and reverted cleanly; the transfer toggle dropped the 2026 spend KPI by exactly the row amount (26'303.21 -> 26'293.85) and restored it; source filter (revolut -> 188 rows, page 1/4), category filter (dining -> 64 rows), `uncategorized` filter (10 rows, grey chips) and pagination (836 rows, page 1/17 for 2025) all match direct API/SQL counts; year switching remounts and refetches the table with the KPIs updating to the selected year; zero console errors. Full `just check` green.
