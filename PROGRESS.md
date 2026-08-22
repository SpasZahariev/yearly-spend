## Issue #2 - Scaffold workspace, DB schema, and empty pixel app

- Added the Rust workspace (`core`, `ingest`, `api`), DuckDB schema, seeded account/category metadata, FX cache, LLM provider config, and `spend` CLI scaffold.
- Added the Axum static frontend server and `GET /api/meta`, plus the Vite/React/Tailwind/shadcn pixel shell with design tokens.
- Fixed first-run database initialization: DuckDB sequences now generate primary keys, table discovery uses `schema_name`, and FX date parameters use explicit casts.
- Added regression coverage for generated IDs, metadata responses, schema initialization, and FX cache reads.
- Wired and verified cargo fmt/clippy/tests plus frontend lint/format/build. Initialized no-mistakes and created the `issue-2-scaffold` branch.
