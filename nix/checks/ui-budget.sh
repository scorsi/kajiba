#!/usr/bin/env bash
# The gzip size of the initial JS, CSS and fonts of the web application stays under a budget
# (J5 task 9). Usage: ui-budget.sh <bundle dir> <budget in bytes>
# shortcut: measures the shell page's initial set only; lazy route chunks are not counted,
# upgrade when a route's own weight becomes a concern.
set -euo pipefail

dir=$1
budget=$2

files=$(grep -oE '(href|import\()="?/_app/[^"]+\.(js|css)' "$dir/index.html" | grep -oE '/_app/.*' | sort -u)
for css in $(grep '\.css$' <<<"$files"); do
  fonts=$(grep -oE '[A-Za-z0-9_.-]+\.woff2' "$dir$css" | sort -u | sed 's|^|/_app/immutable/assets/|')
  files=$(printf '%s\n%s\n' "$files" "$fonts")
done
total=0
for f in $(grep . <<<"$files" | sort -u); do
  size=$(gzip -9 -c "$dir$f" | wc -c)
  total=$((total + size))
done
echo "initial gzip size: $total bytes (budget $budget)"
if [ "$total" -gt "$budget" ]; then
  echo "FAIL: over budget" >&2
  exit 1
fi
