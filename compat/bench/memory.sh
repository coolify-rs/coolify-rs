#!/bin/sh
# Startup time and memory of coolify-rs, from its production image, next to the reference Coolify:
#   compat/bench/memory.sh [image]        default coolify-rs:bench (docker build -t coolify-rs:bench .)
# Runs the image as container coolify-rs-bench on the reference's network and database (port 18095,
# scheduler off), then puts 960 requests of the acme team's applications list over 16 connections
# through each side. Prints the start time, idle memory, peak and memory after the load.
# The load is over Coolify's API rate limit (API_RATE_LIMIT, 200 a minute), so the script raises it in the
# reference's config cache for the run and puts it back at the end.
# Stop it with: docker rm -f coolify-rs-bench
set -e
IMAGE="${1:-coolify-rs:bench}"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
REF=coolify-rs-reference-coolify-1
APP_KEY=$(docker exec "$REF" printenv APP_KEY)
DB_PASS=$(docker exec coolify-rs-reference-postgres-1 printenv POSTGRES_PASSWORD)
DB_USER=$(docker exec coolify-rs-reference-postgres-1 printenv POSTGRES_USER)
DB_NAME=$(docker exec coolify-rs-reference-postgres-1 printenv POSTGRES_DB)
export APP_KEY DATABASE_URL="postgres://$DB_USER:$DB_PASS@coolify-rs-reference-postgres-1:5432/$DB_NAME"
docker rm -f coolify-rs-bench >/dev/null 2>&1 || true
docker create --name coolify-rs-bench --network coolify-rs-reference_default -p 127.0.0.1:18095:8080 \
  -e APP_KEY -e DATABASE_URL -e COOLIFY_RS_SCHEDULER=off -e COOLIFY_RS_SKIP_STARTUP_CLEANUP=1 \
  -e COOLIFY_RS_TEST_ONLY_FAKE_PROVIDERS=http://127.0.0.1:9 -e COOLIFY_RS_CLOUD_API_BASE=http://127.0.0.1:9 \
  "$IMAGE" >/dev/null

now() { python3 -c 'import time; print(time.time())'; }
since() { python3 -c "import time; print('%.2f s' % (time.time() - $1))"; }
mem() { docker stats --no-stream --format '{{.MemUsage}}' "$1" | cut -d/ -f1; }

T0=$(now); docker start coolify-rs-bench >/dev/null
until curl -sf -o /dev/null http://127.0.0.1:18095/api/health; do sleep 0.02; done
echo "coolify-rs start to first healthy response: $(since "$T0")"
docker stop "$REF" >/dev/null
T0=$(now); docker start "$REF" >/dev/null
until [ "$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:18000/api/health)" = 200 ]; do sleep 0.05; done
echo "coolify start to first healthy response: $(since "$T0")"
LIMIT=$(docker exec "$REF" php -r "echo (require 'bootstrap/cache/config.php')['api']['rate_limit'];")
limit() { docker exec "$REF" sed -i "s/'rate_limit' => [0-9]*,/'rate_limit' => $1,/" bootstrap/cache/config.php; }
limit 100000000
trap 'limit "$LIMIT"' EXIT
# Coolify's workers and scheduler finish starting after the first healthy response.
sleep 60
echo "idle: coolify-rs $(mem coolify-rs-bench), coolify $(mem "$REF") + redis $(mem coolify-rs-reference-redis-1)"

load() {
  python3 -I - "$ROOT/compat/reference/secrets/tokens.json" "$1" <<'EOF'
import http.client, json, sys, threading, time
token = json.load(open(sys.argv[1]))['acme_owner_root']
port = int(sys.argv[2])
def worker():
    c = http.client.HTTPConnection('127.0.0.1', port, timeout=120)
    for _ in range(60):
        c.request('GET', '/api/v1/applications', headers={'Authorization': 'Bearer ' + token})
        r = c.getresponse(); r.read(); assert r.status == 200, r.status
t = time.time(); threads = [threading.Thread(target=worker) for _ in range(16)]
[x.start() for x in threads]; [x.join() for x in threads]
print('%.0f req/s' % (960 / (time.time() - t)))
EOF
}
SAMPLES=$(mktemp)
peak() {
  : > "$SAMPLES"
  while :; do mem "$1" >> "$SAMPLES"; done &
  SAMPLER=$!
}
max() { kill "$SAMPLER"; wait "$SAMPLER" 2>/dev/null || true; python3 -c "
import re, sys
u = {'KiB': 1 / 1024, 'MiB': 1, 'GiB': 1024, 'B': 1 / 1048576}
v = [float(m[0]) * u[m[1]] for m in re.findall(r'([\d.]+)(\w+)', open(sys.argv[1]).read())]
print('%.0f MiB' % max(v))" "$SAMPLES"; }

peak coolify-rs-bench; R=$(load 18095); echo "coolify-rs under load: $R, peak $(max), after $(mem coolify-rs-bench)"
peak "$REF"; R=$(load 18000); echo "coolify under load: $R, peak $(max), after $(mem "$REF")"
