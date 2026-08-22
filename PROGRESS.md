## Issue #2 - Scaffold workspace, DB schema, and empty pixel app

- Added the Rust workspace (`core`, `ingest`, `api`), DuckDB schema, seeded account/category metadata, FX cache, LLM provider config, and `spend` CLI scaffold.
- Added the Axum static frontend server and `GET /api/meta`, plus the Vite/React/Tailwind/shadcn pixel shell with design tokens.
- Fixed first-run database initialization: DuckDB sequences now generate primary keys, table discovery uses `schema_name`, and FX date parameters use explicit casts.
- Added regression coverage for generated IDs, metadata responses, schema initialization, and FX cache reads.
- Wired and verified cargo fmt/clippy/tests plus frontend lint/format/build. Created the `issue-2-scaffold` branch.

## Issue #3 - Ingest Neon CSVs idempotently

- Added semicolon-delimited Neon CSV parsing with quoted fields, UTF-8 BOM support, and CRLF-preserving multiline subjects.
- Added natural-key transaction hashing that excludes Neon categories, SHA-based whole-file skips, and sequence-backed idempotent upserts.
- Mapped Neon labels into the fixed taxonomy, batched uncategorized rows through the configured local LLM with taxonomy response validation, and recorded LLM audits.
- Marked negative Neon outflows to Revolut, Swisscard AECS, and Interactive Brokers as `transfer_out`.
- Added parser, taxonomy, transfer, LLM validation, and idempotency regression coverage.
