#!/usr/bin/env bash
# Builds and starts the SSH test host (compat/testing-host) on 127.0.0.1:${COOLIFY_RS_TEST_SSH_PORT:-2223}.
# Generates a throwaway ed25519 key on first run; it never leaves this machine.
# When the reference environment is running, the host also joins its network as `testing-host`.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
dir="$root/compat/testing-host"
key="$dir/secrets/id_ed25519"
port="${COOLIFY_RS_TEST_SSH_PORT:-2223}"

if [[ ! -f "$key" ]]; then
  mkdir -p "$dir/secrets"
  ssh-keygen -q -t ed25519 -N '' -C coolify-rs-test -f "$key"
fi

docker build -q -t coolify-rs-testing-host --build-arg "TEST_PUBLIC_KEY=$(cat "$key.pub")" "$dir" >/dev/null
docker rm -f coolify-rs-testing-host >/dev/null 2>&1 || true
docker run -d --name coolify-rs-testing-host -p "127.0.0.1:$port:22" coolify-rs-testing-host >/dev/null
if docker network inspect coolify-rs-reference_default >/dev/null 2>&1; then
  # One alias per scenario, so the reference can register each as its own server
  # (compat/diff/validate_diff.py).
  aliases=(--alias testing-host)
  for u in root deploy nojq nodocker olddocker badjson gentoo root-ghost; do aliases+=(--alias "th-$u"); done
  docker network connect "${aliases[@]}" coolify-rs-reference_default coolify-rs-testing-host
fi
echo "SSH test host: 127.0.0.1:$port  key: $key"
