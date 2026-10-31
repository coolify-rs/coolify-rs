#!/bin/sh
# Installs coolify-rs on the VM the way a user does: curl install.sh from the publishing address, piped
# to bash. The address is a plain HTTP server inside the VM, and the locally built image goes in through
# COOLIFY_RS_IMAGE_FILE, because nothing is published yet.
#   rs_install.sh <image tag>       tag of coolify-rs/coolify-rs in the local Docker
set -eu
VM=coolify-rs-agent-install-rs
TAG="$1"
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
SCRATCH="${INSTALL_SCRATCH:?set INSTALL_SCRATCH to a private folder under the macOS temp folder}"
mkdir -p "$SCRATCH"

docker save "coolify-rs/coolify-rs:$TAG" -o "$SCRATCH/image-$TAG.tar"
orb -m "$VM" -u root bash "$ROOT/compat/diff/install/rs_cdn.sh" "$ROOT/scripts/install" "$TAG"
orb -m "$VM" -u root bash "$ROOT/compat/diff/install/rs_run_installer.sh" "$SCRATCH/image-$TAG.tar"
