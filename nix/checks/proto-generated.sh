#!/usr/bin/env bash
# Run from the repository root with protoc-gen-es on PATH (0020).
set -euo pipefail

repo=$PWD
generated=$(mktemp -d)
trap 'rm -rf "$generated"' EXIT
cp -R proto "$generated/proto"
mkdir -p "$generated/apps/ui/src/lib/gen"
(
  cd "$generated/proto"
  buf generate
)
diff -ru "$repo/apps/ui/src/lib/gen" "$generated/apps/ui/src/lib/gen"
