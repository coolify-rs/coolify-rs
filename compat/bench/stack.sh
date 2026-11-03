#!/bin/sh
# Two isolated benchmark stacks, each shaped like a small VPS:
#   compat/bench/stack.sh up [coolify-rs image]    default coolify-rs:bench (docker build -t coolify-rs:bench .)
#   compat/bench/stack.sh down
# Stack "coolify": the official image with its own PostgreSQL and Redis. Stack "coolify-rs": the image built
# from this repository with its own PostgreSQL. Each stack is pinned to its own 2 CPU cores, with 4 GB for the
# application, and both run their schedulers and workers as in production. Both databases start as copies
# of the reference database (compat/reference), so the data and API tokens are identical.
# The network is internal: nothing in either stack can reach the internet or the servers in the data.
set -e
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
NET=coolify-rs-bench
COOLIFY_IMAGE="coollabsio/coolify:sha-b5823c7"
RS_IMAGE="${2:-coolify-rs:bench}"
COOLIFY_CPUS=2,3
RS_CPUS=4,5
REF=coolify-rs-reference-coolify-1
REF_DB=coolify-rs-reference-postgres-1

down() {
  docker rm -f bench-coolify bench-coolify-db bench-coolify-redis bench-rs bench-rs-db >/dev/null 2>&1 || true
  docker network rm "$NET" >/dev/null 2>&1 || true
}

# db <name> <cpus>: PostgreSQL 15 holding a copy of the reference database.
db() {
  docker run -d --name "$1" --network "$NET" --cpuset-cpus "$2" --memory 1g \
    -e POSTGRES_USER=coolify -e POSTGRES_DB=coolify -e POSTGRES_PASSWORD="$DB_PASSWORD" \
    postgres:15-alpine >/dev/null
  until docker exec "$1" pg_isready -q -U coolify -d coolify 2>/dev/null; do sleep 0.5; done
  # The image's init restarts the server once; wait for the final one.
  sleep 2
  until docker exec "$1" pg_isready -q -U coolify -d coolify 2>/dev/null; do sleep 0.5; done
  docker exec -i "$1" pg_restore -U coolify -d coolify --no-owner --exit-on-error < "$DUMP"
}

health() {
  until docker exec "$1" curl -sf -o /dev/null http://127.0.0.1:8080/api/health 2>/dev/null; do sleep 0.5; done
}

case "$1" in
up)
  down
  docker network create --internal "$NET" >/dev/null
  DUMP=$(mktemp)
  trap 'rm -f "$DUMP"' EXIT
  docker exec "$REF_DB" sh -c 'pg_dump -Fc -U "$POSTGRES_USER" -d "$POSTGRES_DB"' > "$DUMP"
  DB_PASSWORD=$(openssl rand -hex 24)
  # The data is encrypted with the reference's key, so both stacks use it.
  APP_KEY=$(docker exec "$REF" printenv APP_KEY)
  export DB_PASSWORD APP_KEY
  db bench-coolify-db "$COOLIFY_CPUS"
  db bench-rs-db "$RS_CPUS"

  docker run -d --name bench-coolify-redis --network "$NET" --cpuset-cpus "$COOLIFY_CPUS" --memory 256m \
    redis:7-alpine redis-server --save "" --loglevel warning >/dev/null
  # API_RATE_LIMIT: Coolify allows 200 API requests a minute per user by default; coolify-rs has no limit.
  docker run -d --name bench-coolify --network "$NET" --cpuset-cpus "$COOLIFY_CPUS" --memory 4g \
    -e APP_ENV=production -e APP_NAME=Coolify -e APP_URL=http://bench-coolify:8080 -e APP_KEY \
    -e APP_ID="$(openssl rand -hex 16)" -e SELF_HOSTED=true -e AUTOUPDATE=false -e API_RATE_LIMIT=1000000000 \
    -e DB_HOST=bench-coolify-db -e DB_PORT=5432 -e DB_DATABASE=coolify -e DB_USERNAME=coolify -e DB_PASSWORD \
    -e REDIS_HOST=bench-coolify-redis -e REDIS_PASSWORD= \
    -e PUSHER_APP_ID="$(openssl rand -hex 16)" -e PUSHER_APP_KEY="$(openssl rand -hex 16)" \
    -e PUSHER_APP_SECRET="$(openssl rand -hex 16)" \
    "$COOLIFY_IMAGE" >/dev/null
  docker run -d --name bench-rs --network "$NET" --cpuset-cpus "$RS_CPUS" --memory 4g \
    -e APP_KEY -e DATABASE_URL="postgres://coolify:$DB_PASSWORD@bench-rs-db:5432/coolify" \
    "$RS_IMAGE" >/dev/null
  health bench-rs
  health bench-coolify
  echo "up: bench-coolify (cpus $COOLIFY_CPUS) and bench-rs (cpus $RS_CPUS) on internal network $NET"
  ;;
down)
  down
  ;;
*)
  echo "usage: $0 up [image] | down" >&2
  exit 2
  ;;
esac
