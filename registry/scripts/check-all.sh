#!/usr/bin/env bash
# Rebuilds and conformance-checks every version `coolify-rs plugin review --out <dir>` listed.
#
#   scripts/check-all.sh <dir>
#
# Writes check.json next to each component and appends the results to $GITHUB_STEP_SUMMARY when set.
set -euo pipefail
out=${1:?usage: check-all.sh <review output folder>}
here=$(cd "$(dirname "$0")" && pwd)
failed=0
shopt -s nullglob
for dir in "$out"/builds/*/; do
  dir=${dir%/}
  name=$(basename "$dir")
  if ! "$here/rebuild.sh" "$dir"; then
    failed=1
    continue
  fi
  if coolify-rs plugin check "$dir/component.wasm" "$dir/plugin.toml" > "$dir/check.json"; then
    result="passed"
  else
    result="FAILED"
    failed=1
  fi
  summary=$(jq -r '"memory \(.max_memory) bytes, slowest call \(.max_elapsed_ms) ms" + (if (.errors|length) > 0 then "; errors: " + (.errors|join("; ")) else "" end) + (if (.warnings|length) > 0 then "; notes: " + (.warnings|join("; ")) else "" end)' "$dir/check.json")
  echo "$name: conformance $result ($summary)"
  if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    echo "- \`$name\`: conformance $result ($summary)" >> "$GITHUB_STEP_SUMMARY"
  fi
done
exit $failed
