#!/usr/bin/env bash
# Exercise the real freshness check on temporary copies, without editing the checkout.
set -euo pipefail

check=$1
repo=$PWD
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT
cp -R proto "$fixture/proto"
mkdir -p "$fixture/apps/ui/src/lib"
cp -R apps/ui/src/lib/gen "$fixture/apps/ui/src/lib/gen"
cd "$fixture"

bash "$check"
echo "PASS: fresh generated code"
for fault in changed missing extra invalid-proto; do
  rm -rf apps/ui/src/lib/gen
  cp -R "$repo/apps/ui/src/lib/gen" apps/ui/src/lib/gen
  case "$fault" in
    changed) echo '// stale' >> apps/ui/src/lib/gen/kajiba/v1/health_pb.ts ;;
    missing) rm apps/ui/src/lib/gen/kajiba/v1/health_pb.ts ;;
    extra) echo '// obsolete' > apps/ui/src/lib/gen/obsolete.ts ;;
    invalid-proto) echo 'invalid proto' >> proto/kajiba/v1/health.proto ;;
  esac
  if bash "$check" > "$fixture/output" 2>&1; then
    echo "FAIL: accepted $fault" >&2
    exit 1
  fi
  echo "PASS: rejected $fault"
done
