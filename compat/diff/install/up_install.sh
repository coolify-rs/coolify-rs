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
# Until 4.5.x the tree carries versions.json. From 4.6.0 on it is generated for the CDN only, and install.sh reads
# it from https://cdn.coollabs.io/coolify/versions.json whatever CDN says.
if [ -f "$SNAP/versions.json" ]; then cp "$SNAP/versions.json" "$STAGE/"; fi
sed -i '' 's#^CDN=.*#CDN="file:///root/cdn"#' "$STAGE/install.sh" "$STAGE/upgrade.sh"
tar -C "$STAGE" -cf - . | orb -m "$VM" -u root bash -c 'mkdir -p /root/cdn && tar -C /root/cdn -xf -'
orb -m "$VM" -u root env ROOT_USER_EMAIL="${ROOT_USER_EMAIL:-}" ROOT_USER_PASSWORD="${ROOT_USER_PASSWORD:-}" \
    bash -c "cd /root && USER=root bash /root/cdn/install.sh $VERSION"
