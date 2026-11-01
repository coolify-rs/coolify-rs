#!/bin/sh
# Runs coolify-rs in a Linux container on the reference's Docker network:
#   scripts/dev-server-container.sh [port]    stop with: docker rm -f coolify-rs-dev-<port>
# Names on that network resolve as they do for the reference (the trusted `coolify-minio`), and the
# instance has real ssh to the test VMs. APP_KEY and the database password are never printed.
set -e
PORT="${1:-18090}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
NAME="coolify-rs-dev-$PORT"
NET=coolify-rs-reference_default
APP_KEY_VALUE=$(docker exec coolify-rs-reference-coolify-1 printenv APP_KEY)
DB_PASS=$(docker exec coolify-rs-reference-postgres-1 printenv POSTGRES_PASSWORD)
DB_USER=$(docker exec coolify-rs-reference-postgres-1 printenv POSTGRES_USER)
DB_NAME=$(docker exec coolify-rs-reference-postgres-1 printenv POSTGRES_DB)
docker rm -f "$NAME" >/dev/null 2>&1 || true
docker run --rm -v "$ROOT:/src" -v "$NAME-target:/target" -v "$NAME-cargo:/usr/local/cargo/registry" \
  -e CARGO_TARGET_DIR=/target -w /src rust:1-bookworm cargo build -q -p coolify-rs
APP_KEY="$APP_KEY_VALUE" DATABASE_URL="postgres://$DB_USER:$DB_PASS@coolify-rs-reference-postgres-1:5432/$DB_NAME" \
  docker run -d --name "$NAME" --network "$NET" -p "127.0.0.1:$PORT:$PORT" \
  -v "$ROOT/frontend/dist:/ui:ro" -v "$NAME-target:/target:ro" -e APP_KEY -e DATABASE_URL \
  -e COOLIFY_RS_LISTEN="0.0.0.0:$PORT" -e COOLIFY_RS_UI_DIR=/ui -e COOLIFY_RS_DATA_DIR=/data \
  -e COOLIFY_RS_SKIP_STARTUP_CLEANUP=1 -e COOLIFY_RS_SCHEDULER=off \
  rust:1-bookworm /target/debug/coolify-rs serve >/dev/null
sleep 3
curl -s -o /dev/null -w "port $PORT: %{http_code}, in container $NAME\n" "http://127.0.0.1:$PORT/login"
