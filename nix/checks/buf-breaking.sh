#!/usr/bin/env bash
# `buf breaking` of proto/ against the last release's proto/ (ADRs 0020, 0101).
# Usage: buf-breaking.sh <proto dir> <baseline proto dir, empty before the first release> <version>
set -euo pipefail

proto=$1
baseline=$2
version=$3

if [ -z "$baseline" ]; then
  echo "buf-breaking: no release baseline"
  exit 0
fi
status=0
(cd "$proto" && buf breaking --against "$baseline") || status=$?
case "$status" in
  0) echo "buf-breaking: no breaking change against the last release" ;;
  # buf exits 100 when it reports breaking changes; anything else is a failed run.
  100)
    if [ "${version%%.*}" -ge 1 ]; then
      echo "buf-breaking: breaking change from 1.0 on: add a new package instead (ADR 0020)" >&2
      exit 1
    fi
    echo "buf-breaking: informational before 1.0 (ADR 0020): the changes above are allowed in a minor bump"
    ;;
  *) exit "$status" ;;
esac
