#!/usr/bin/env bash
# Exercise the breaking check on temporary copies of proto/.
# Usage: buf-breaking-test.sh <check script> <proto dir>
set -euo pipefail

check=$1
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT
cp -R "$2" "$fixture/current"
cp -R "$2" "$fixture/baseline"
chmod -R u+w "$fixture"

expect() {
  local want=$1 message=$2
  shift 2
  local got=0
  bash "$check" "$@" > "$fixture/output" 2>&1 || got=$?
  if [ "$got" != "$want" ] || ! grep -q "$message" "$fixture/output"; then
    echo "FAIL: $* exited $got, want $want and '$message':" >&2
    cat "$fixture/output" >&2
    exit 1
  fi
  echo "PASS: $message"
}

expect 0 "no release baseline" "$fixture/current" "" 0.1.0
expect 0 "no breaking change" "$fixture/current" "$fixture/baseline" 1.0.0
# The release had a field the current contract removed.
sed -i.orig 's/^message GetViewerRequest {}$/message GetViewerRequest { string removed = 1; }/' \
  "$fixture/baseline/kajiba/v1/auth.proto"
if cmp -s "$fixture/baseline/kajiba/v1/auth.proto" "$fixture/baseline/kajiba/v1/auth.proto.orig"; then
  echo "FAIL: the fixture edit did not apply" >&2
  exit 1
fi
rm "$fixture/baseline/kajiba/v1/auth.proto.orig"
expect 0 "informational before 1.0" "$fixture/current" "$fixture/baseline" 0.9.0
expect 1 "breaking change from 1.0 on" "$fixture/current" "$fixture/baseline" 1.0.0
# A baseline buf cannot read fails the check, even before 1.0.
if bash "$check" "$fixture/current" "$fixture/missing" 0.1.0 > "$fixture/output" 2>&1; then
  echo "FAIL: accepted an unreadable baseline" >&2
  exit 1
fi
echo "PASS: unreadable baseline refused"
