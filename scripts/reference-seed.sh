#!/usr/bin/env bash
# Seeds fixture users/teams/tokens into the reference instance through upstream's
# own models and stores the tokens in compat/reference/secrets/tokens.json.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
c=coolify-rs-reference-coolify-1
docker cp "$root/compat/reference/seed-fixtures.php" "$c:/tmp/seed-fixtures.php"
out="$(docker exec -w /var/www/html "$c" php artisan tinker --execute="require '/tmp/seed-fixtures.php';")"
umask 077
printf '%s\n' "$out" | sed -n '/^{/,/^}/p' > "$root/compat/reference/secrets/tokens.json"
python3 -c "import json,sys; d=json.load(open(sys.argv[1])); print('seeded tokens:', ', '.join(k for k in d if not k.endswith('_id')))" "$root/compat/reference/secrets/tokens.json"
