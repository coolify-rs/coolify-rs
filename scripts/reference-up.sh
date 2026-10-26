#!/usr/bin/env bash
# Starts the upstream Coolify reference environment (compat/reference).
# Generates throwaway secrets on first run; they never leave this machine.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
ref="$root/compat/reference"
secrets="$ref/secrets/reference.envfile"

if [[ ! -f "$secrets" ]]; then
  mkdir -p "$ref/secrets"
  umask 077
  db_password="$(openssl rand -hex 24)"
  cat >"$secrets" <<EOF
APP_ID=$(openssl rand -hex 16)
APP_NAME=Coolify
APP_KEY=base64:$(openssl rand -base64 32)
DB_USERNAME=coolify
DB_DATABASE=coolify
DB_PASSWORD=$db_password
POSTGRES_USER=coolify
POSTGRES_DB=coolify
POSTGRES_PASSWORD=$db_password
REDIS_PASSWORD=
PUSHER_APP_ID=$(openssl rand -hex 16)
PUSHER_APP_KEY=$(openssl rand -hex 16)
PUSHER_APP_SECRET=$(openssl rand -hex 16)
EOF
  echo "generated $secrets"
fi

docker compose -f "$ref/compose.yaml" up -d --wait
echo "reference Coolify: http://127.0.0.1:18000  postgres: 127.0.0.1:15432"
