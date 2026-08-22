set dotenv-load := true

rust_linker := env_var_or_default("RUST_LINKER", env("HOME", "/home/spas") + "/.local/bin/gcc")
export RUSTFLAGS := env_var_or_default("RUSTFLAGS", "-C linker=" + rust_linker)

default:
    @just --list

# Create .env, install frontend dependencies, and build the full stack.
setup: ensure-env frontend-install build

ensure-env:
    @if [ -f .env ]; then \
      echo ".env already exists"; \
    else \
      cp .env.example .env; \
      echo "Created .env from .env.example"; \
    fi

# Build Rust and frontend artifacts.
build: rust-build frontend-build

rust-build:
    cargo build

rust-test:
    cargo test --workspace

rust-clippy:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

rust-fmt:
    cargo fmt --all

rust-fmt-check:
    cargo fmt --all -- --check

frontend-install:
    pnpm --dir frontend install --frozen-lockfile

frontend-build:
    pnpm --dir frontend build

frontend-lint:
    pnpm --dir frontend lint

frontend-format:
    pnpm --dir frontend format

frontend-format-check:
    pnpm --dir frontend format:check

frontend-check: frontend-lint frontend-format-check frontend-build

# Run all local checks without the external gate.
check: rust-fmt-check rust-clippy rust-test frontend-check

# Build the frontend and start the Axum server at http://127.0.0.1:3000.
serve: frontend-build
    cargo run -p spend-api

run: serve

# Start the Vite development server for frontend-only work.
frontend-dev:
    pnpm --dir frontend dev

# Discover CSV statements and initialize the database.
ingest path="statements/":
    cargo run -p ingest -- ingest "{{ path }}"

# Query the running API metadata endpoint.
meta:
    curl --fail --silent --show-error http://127.0.0.1:3000/api/meta
