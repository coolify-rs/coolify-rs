#!/bin/bash
# Runs on a VM: one fingerprint of the generated secrets and the ssh key, to compare before and after a
# rerun or an upgrade without printing them.
set -eu
for key in APP_ID APP_KEY DB_PASSWORD REDIS_PASSWORD PUSHER_APP_ID PUSHER_APP_KEY PUSHER_APP_SECRET; do
    docker inspect coolify --format '{{range .Config.Env}}{{println .}}{{end}}' | grep "^$key=" | sha256sum | cut -c1-16
done | sha256sum | cut -c1-32
docker exec coolify-db psql -U coolify -d coolify -Atc "SELECT md5(private_key) || fingerprint FROM private_keys WHERE id = 0" | sed 's/^[0-9a-f]*//' | sha256sum | cut -c1-32
grep -c ' coolify$' /root/.ssh/authorized_keys
