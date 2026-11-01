#!/bin/sh
# Runs upstream's own install.sh on the VM at the baseline version.
#
# install.sh can pin the image (its first argument) but always downloads the compose files, the
# environment template and upgrade.sh from the CDN's current release. So the snapshot's installer runs
# with one line changed: CDN points at a folder holding the snapshot's copies of those files.
set -eu
VM=coolify-rs-agent-install-up
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
SNAP="$ROOT/.upstream/snapshot"
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
cp "$SNAP/docker-compose.yml" "$SNAP/docker-compose.prod.yml" "$SNAP/.env.production" "$SNAP/versions.json" \
    "$SNAP/scripts/install.sh" "$SNAP/scripts/upgrade.sh" "$SNAP/scripts/upgrade-postgres.sh" "$STAGE/"
sed -i '' 's#^CDN=.*#CDN="file:///root/cdn"#' "$STAGE/install.sh" "$STAGE/upgrade.sh"
tar -C "$STAGE" -cf - . | orb -m "$VM" -u root bash -c 'mkdir -p /root/cdn && tar -C /root/cdn -xf -'
orb -m "$VM" -u root bash -c 'cd /root && USER=root bash /root/cdn/install.sh sha-b5823c7'
