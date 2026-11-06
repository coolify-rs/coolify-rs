#!/usr/bin/env bash
# Installs the coolify-rs CLI the checks run with, built from a pinned commit of coolify-rs.
# The pinned version decides which plugin interface and capabilities the registry accepts, and which
# registry keys `coolify-rs plugin verify-index` trusts.
set -euo pipefail
: "${COOLIFY_RS_REPO:?set COOLIFY_RS_REPO}"
: "${COOLIFY_RS_REF:?set COOLIFY_RS_REF to a commit}"
if ! [[ "$COOLIFY_RS_REF" =~ ^[0-9a-f]{40}$ ]]; then
  echo "::error::COOLIFY_RS_REF must be a full coolify-rs commit hash, not $COOLIFY_RS_REF" >&2
  exit 1
fi
if command -v coolify-rs >/dev/null && [ "$(cat "$HOME/.cargo/coolify-rs.ref" 2>/dev/null)" = "$COOLIFY_RS_REF" ]; then
  echo "coolify-rs $COOLIFY_RS_REF is installed"
  exit 0
fi
cargo install --locked --force --git "$COOLIFY_RS_REPO" --rev "$COOLIFY_RS_REF" coolify-rs
echo "$COOLIFY_RS_REF" > "$HOME/.cargo/coolify-rs.ref"
