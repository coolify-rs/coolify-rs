#!/usr/bin/env bash
# Runs the full local verification: fmt, clippy, and all tests against the dev
# Postgres from compose.yaml (started if needed).
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
docker compose up -d --wait postgres >/dev/null
export COOLIFY_RS_TEST_DATABASE_URL="${COOLIFY_RS_TEST_DATABASE_URL:-postgres://coolify_rs:coolify_rs@127.0.0.1:25432/coolify_rs}"
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features "$@"
