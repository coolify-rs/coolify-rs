#!/bin/bash
# Runs on the coolify-rs VM: with the database unreachable, upgrade.sh must stop before it changes anything.
set -u
BEFORE=$(docker inspect coolify --format '{{.Id}} {{.Config.Image}}')
docker pause coolify-db >/dev/null
timeout 60 bash /data/coolify/source/upgrade.sh 9.9.9 1.0.19 docker.io >/dev/null 2>&1
CODE=$?
docker unpause coolify-db >/dev/null
AFTER=$(docker inspect coolify --format '{{.Id}} {{.Config.Image}}')
STATUS=$(cut -d'|' -f1,2 /data/coolify/source/.upgrade-status)
rm -f /data/coolify/source/.upgrade-status
echo "exit=$CODE status=$STATUS"
[ "$BEFORE" = "$AFTER" ] && echo "container unchanged" || echo "CONTAINER CHANGED"
