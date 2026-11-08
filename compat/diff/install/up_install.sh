#!/bin/sh
# Runs upstream's own install.sh on the VM at the baseline version.
#   UP_SNAPSHOT=<folder> UP_VERSION=<image tag> up_install.sh    another upstream release (default: the baseline)
#   ROOT_USER_EMAIL, ROOT_USER_PASSWORD                         passed to the installer when set
#
# install.sh can pin the image (its first argument) but always downloads the compose files, the
# environment template and upgrade.sh from the CDN's current release. So the snapshot's installer runs
# with one line changed: CDN points at a folder holding the snapshot's copies of those files.
set -eu
VM=coolify-rs-agent-install-up
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
SNAP="${UP_SNAPSHOT:-$ROOT/.upstream/snapshot}"
VERSION="${UP_VERSION:-sha-b5823c7}"
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
cp "$SNAP/docker-compose.yml" "$SNAP/docker-compose.prod.yml" "$SNAP/.env.production" \
    "$SNAP/scripts/install.sh" "$SNAP/scripts/upgrade.sh" "$SNAP/scripts/upgrade-postgres.sh" "$STAGE/"
# Until 4.5.x the tree carries versions.json. From 4.6.0 on the CDN's versions.json is generated from release.json
# and upstream.json, one section per update channel, and install.sh reads it from cdn.coollabs.io whatever CDN says
# (the VM blocks that host). Build the stable section the same way and point the installer at it.
if [ -f "$SNAP/versions.json" ]; then
    cp "$SNAP/versions.json" "$STAGE/"
else
    python3 - "$SNAP" "$STAGE/versions.json" <<'PY'
import json, sys
snap, out = sys.argv[1], sys.argv[2]
release = json.load(open(f"{snap}/release.json"))
upstream = json.load(open(f"{snap}/upstream.json"))
stable = {"coolify": {"version": release["stable"]["coolify"]}, **upstream}
json.dump({"stable": stable}, open(out, "w"), indent=4)
PY
fi
sed -i '' 's#https://cdn.coollabs.io/coolify/versions.json#file:///root/cdn/versions.json#' "$STAGE/install.sh"
sed -i '' 's#^CDN=.*#CDN="file:///root/cdn"#' "$STAGE/install.sh" "$STAGE/upgrade.sh"
tar -C "$STAGE" -cf - . | orb -m "$VM" -u root bash -c 'mkdir -p /root/cdn && tar -C /root/cdn -xf -'
orb -m "$VM" -u root env ROOT_USER_EMAIL="${ROOT_USER_EMAIL:-}" ROOT_USER_PASSWORD="${ROOT_USER_PASSWORD:-}" \
    bash -c "cd /root && USER=root bash /root/cdn/install.sh $VERSION"
