#!/usr/bin/env bash
set -euo pipefail

cd "$1"
# Split terms so the check does not match its own source.
pattern='ishi''zue|ji''ban|tail''scale|forge''jo|sekkei''zu|ki''so|kan''na|rireki''sho|scorsi''\.dev|uchi''\.'
status=0
while IFS= read -r -d '' file; do
  filter=''
  if [[ "$file" == ./flake.nix ]]; then
    filter='s@github:scorsi/[[:alnum:]_./?=&%-]+@@g'
  fi
  if matches=$(sed -E "$filter" "$file" | grep -niE "$pattern"); then
    printf '%s:\n%s\n' "$file" "$matches" >&2
    status=1
  elif [[ $? != 1 ]]; then
    exit 2
  fi
done < <(find . -type f ! -path './docs/*' -print0)
exit "$status"
