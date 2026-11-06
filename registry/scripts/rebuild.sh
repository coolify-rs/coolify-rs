#!/usr/bin/env bash
# Rebuilds one submitted plugin version from its pinned commit in its pinned builder image, and checks
# that the result and the published component both have the SHA-256 the registry entry names.
#
#   scripts/rebuild.sh <folder>
#
# <folder> is one of the folders `coolify-rs plugin review --out <dir>` writes under <dir>/builds: it holds
# release.json and plugin.toml. On success the rebuilt component is left there as component.wasm.
set -euo pipefail

dir=${1:?usage: rebuild.sh <build folder>}
release="$dir/release.json"
field() { jq -er "$1" "$release"; }

id=$(field .id)
version=$(field .version)
source=$(field .source)
commit=$(field .commit)
builder=$(field .builder)
build_dir=$(field .build.dir)
command=$(field .build.command)
artifact=$(field .build.artifact)
expected=$(field .sha256)
url=$(field .url)

echo "::group::$id $version: rebuilding $source@$commit in $builder"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
git init -q "$work/src"
git -C "$work/src" fetch -q --depth 1 "$source" "$commit"
git -C "$work/src" -c advice.detachedHead=false checkout -q FETCH_HEAD
test "$(git -C "$work/src" rev-parse HEAD)" = "$commit"

# The same paths every time, so nothing machine-specific ends up in the component.
docker run --rm \
  -v "$work/src:/build/src" \
  -w "/build/src/$build_dir" \
  -e CARGO_HOME=/build/cargo-home \
  -e SOURCE_DATE_EPOCH=0 \
  "$builder" sh -euc "$command"
built="$work/src/$build_dir/$artifact"
echo "::endgroup::"

actual=$(sha256sum "$built" | cut -d' ' -f1)
if [ "$actual" != "$expected" ]; then
  echo "::error::$id $version: the reproducible build has SHA-256 $actual, the entry says $expected"
  exit 1
fi

# The published file must be exactly what was rebuilt. 20 MiB is the largest component coolify-rs accepts.
curl -fsSL --proto '=https' --max-redirs 3 --max-filesize 20971520 "$url" -o "$work/published.wasm"
published=$(sha256sum "$work/published.wasm" | cut -d' ' -f1)
if [ "$published" != "$expected" ]; then
  echo "::error::$id $version: $url has SHA-256 $published, the entry says $expected"
  exit 1
fi

cp "$built" "$dir/component.wasm"
echo "$id $version: reproducible, SHA-256 $actual"
