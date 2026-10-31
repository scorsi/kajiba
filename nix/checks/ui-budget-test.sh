#!/usr/bin/env bash
set -euo pipefail

check=$1
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT
mkdir -p "$fixture/_app/immutable/entry"
printf '<link href="/_app/immutable/entry/start.js" rel="modulepreload">\n' > "$fixture/index.html"
head -c 100000 /dev/urandom > "$fixture/_app/immutable/entry/start.js"

bash "$check" "$fixture" 300000
echo "PASS: small bundle"
if bash "$check" "$fixture" 1000; then
  echo "FAIL: accepted an oversized bundle" >&2
  exit 1
fi
echo "PASS: oversized bundle rejected"
