#!/bin/bash
# Runs on the VM: publishes scripts/install at http://host.docker.internal:8899 with $2 as the newest
# version. The host resolves that name to itself and the containers to the Docker gateway.
set -eu
SOURCE="$1"
VERSION="$2"
CDN_DIR=/root/rs-cdn
mkdir -p "$CDN_DIR"
cp "$SOURCE"/* "$CDN_DIR"/
# Only the first version line, coolify.v4.version.
sed -i "0,/\"version\": \"[^\"]*\"/s//\"version\": \"$VERSION\"/" "$CDN_DIR/versions.json"
grep -q host.docker.internal /etc/hosts || echo "127.0.0.1 host.docker.internal" >>/etc/hosts
command -v python3 >/dev/null || { apt-get update -y -qq >/dev/null; apt-get install -y -qq python3-minimal >/dev/null; }
if ! pgrep -f "http.server 8899" >/dev/null; then
    cd "$CDN_DIR"
    nohup python3 -m http.server 8899 --bind 0.0.0.0 >/root/rs-cdn.log 2>&1 &
    sleep 1
fi
curl -fsS http://host.docker.internal:8899/versions.json | sed -n 4p
