#!/bin/bash
# Runs on the coolify-rs VM: replaces coolify-rs with upstream's image at the same baseline, on the same
# database, secrets and folders, using upstream's own Compose files from $1.
set -eu
UP="$1"
SOURCE=/data/coolify/source
PSQL="docker exec coolify-db psql -U coolify -d coolify -Atc"
counts() {
    $PSQL "SELECT (SELECT count(*) FROM migrations), (SELECT count(*) FROM users), (SELECT count(*) FROM teams), (SELECT count(*) FROM servers), (SELECT count(*) FROM private_keys), (SELECT count(*) FROM applications), (SELECT count(*) FROM oauth_settings), (SELECT count(*) FROM shared_environment_variables)"
}
echo "rows before: $(counts)"
docker stop coolify >/dev/null
docker rm coolify >/dev/null
LATEST_IMAGE=sha-b5823c7 REGISTRY_URL=docker.io docker compose --project-name source --project-directory "$SOURCE" --env-file "$SOURCE/.env" \
    -f "$UP/docker-compose.yml" -f "$UP/docker-compose.prod.yml" -f "$SOURCE/docker-compose.custom.yml" \
    up -d --wait --wait-timeout 180 >/dev/null 2>&1 || echo "compose did not report healthy"
for _ in $(seq 1 60); do
    [ "$(docker inspect coolify --format '{{.State.Health.Status}}' 2>/dev/null)" = "healthy" ] && break
    sleep 5
done
echo "image $(docker inspect coolify --format '{{.Config.Image}}') health $(docker inspect coolify --format '{{.State.Health.Status}}')"
echo "rows after:  $(counts)"
echo "pending migrations: $(docker exec coolify php artisan migrate:status --pending --no-ansi 2>&1 | grep -c Pending || true)"
docker exec coolify php artisan tinker --execute '
$s = App\Models\Server::find(0);
echo "ssh as upstream: " . trim(instant_remote_process(["echo reached"], $s)) . PHP_EOL;
echo "key decrypts: " . (str_contains($s->privateKey->private_key, "PRIVATE KEY") ? "yes" : "no") . PHP_EOL;
echo "sentinel token decrypts: " . (json_decode(decrypt($s->settings->sentinel_token, false) ?: "{}") ? "yes" : "unparsed") . PHP_EOL;
echo "shared variable decrypts: " . (App\Models\SharedEnvironmentVariable::where("key","COOLIFY_SERVER_UUID")->first()->value === $s->uuid ? "yes" : "no") . PHP_EOL;
' 2>&1 | grep -v '^$' | cut -c1-200
