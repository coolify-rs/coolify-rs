#!/bin/sh
# Starts (or restarts) a coolify-rs dev instance against the reference database.
#   scripts/dev-server.sh [port]      default 18080; run from any checkout or worktree
# Each port has its own binary copy and data directory, so instances can run side by side.
# Hosted providers (Cloudflare, Doppler, Have I Been Pwned) go to COOLIFY_RS_TEST_ONLY_FAKE_PROVIDERS;
# by default a closed local port, so a dev instance never reaches a real one.
# APP_KEY and the database password come from the reference containers and are never printed.
set -e
PORT="${1:-18080}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DIR="$HOME/.cache/coolify-rs-dev/$PORT"
mkdir -p "$DIR/data"
cd "$ROOT"
PID=$(pgrep -f "$DIR/coolify-rs serve" | head -1 || true)
if [ -n "$PID" ]; then kill "$PID"; sleep 1; fi
cargo build -q -p coolify-rs
cp target/debug/coolify-rs "$DIR/coolify-rs"
APP_KEY_VALUE=$(docker exec coolify-rs-reference-coolify-1 printenv APP_KEY)
DB_PASS=$(docker exec coolify-rs-reference-postgres-1 printenv POSTGRES_PASSWORD)
DB_USER=$(docker exec coolify-rs-reference-postgres-1 printenv POSTGRES_USER)
DB_NAME=$(docker exec coolify-rs-reference-postgres-1 printenv POSTGRES_DB)
# Use real ssh when the test VM is reachable, otherwise go through orb.
if ! nc -G 2 -z coolify-rs-e2e.orb.local 22 2>/dev/null; then
  export COOLIFY_RS_SSH_BINARY="${COOLIFY_RS_SSH_BINARY:-$ROOT/scripts/ssh-via-orb.sh}"
fi
env APP_KEY="$APP_KEY_VALUE" DATABASE_URL="postgres://$DB_USER:$DB_PASS@127.0.0.1:15432/$DB_NAME" \
  COOLIFY_RS_LISTEN="127.0.0.1:$PORT" COOLIFY_RS_UI_DIR="$ROOT/frontend/dist" COOLIFY_RS_DATA_DIR="$DIR/data" \
  COOLIFY_RS_SKIP_STARTUP_CLEANUP=1 COOLIFY_RS_SCHEDULER="${COOLIFY_RS_SCHEDULER:-off}" \
  COOLIFY_RS_TEST_ONLY_FAKE_PROVIDERS="${COOLIFY_RS_TEST_ONLY_FAKE_PROVIDERS:-http://127.0.0.1:9}" \
  nohup "$DIR/coolify-rs" serve >"$DIR/server.log" 2>&1 &
sleep 3
curl -s -o /dev/null -w "port $PORT: %{http_code}, log $DIR/server.log\n" "http://127.0.0.1:$PORT/login"
