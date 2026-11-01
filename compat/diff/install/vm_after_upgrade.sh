#!/bin/bash
# Runs on the coolify-rs VM after an upgrade: what must hold.
set -u
echo "image $(docker inspect coolify --format '{{.Config.Image}}') health $(docker inspect coolify --format '{{.State.Health.Status}}')"
echo "version $(docker inspect coolify --format '{{range .Config.Env}}{{println .}}{{end}}' | grep '^COOLIFY_VERSION=')"
BACKUP=$(ls -1 /data/coolify/backups/coolify-rs-upgrades/pre-upgrade-*.dump | tail -1)
echo "backup $(basename "$BACKUP" | sed 's/[0-9-]*\.dump/<date>.dump/') mode $(stat -c '%a %u' "$BACKUP")"
docker exec -i coolify-db pg_restore --list <"$BACKUP" | grep -c 'TABLE DATA'
PSQL="docker exec coolify-db psql -U coolify -d coolify -Atc"
echo "users $($PSQL 'SELECT count(*) FROM users') applications $($PSQL 'SELECT count(*) FROM applications') migrations $($PSQL 'SELECT count(*) FROM migrations')"
docker ps --format '{{.Names}}' | grep -c -- '-[0-9]*$' || true
echo "status file: $(cat /data/coolify/source/.upgrade-status 2>/dev/null)"
