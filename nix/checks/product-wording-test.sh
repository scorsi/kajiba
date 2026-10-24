#!/usr/bin/env bash
set -euo pipefail

check=$1
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT
mkdir -p "$fixture/docs" "$fixture/nested/docs"

expect_rejected() {
  if bash "$check" "$fixture"; then
    echo "FAIL: accepted $1" >&2
    exit 1
  fi
  echo "PASS: rejected $1"
}

bash "$check" "$fixture"
echo "PASS: empty source tree"
for term in 'ishi''zue' 'ji''ban' 'Tail''scale' 'Forge''jo' 'sekkei''zu' 'ki''so' 'kan''na' 'rireki''sho' 'scorsi''.dev' 'uchi''.'; do
  printf '%s\n' "$term" > "$fixture/.comment with spaces"
  expect_rejected "$term"
done
rm "$fixture/.comment with spaces"

printf 'JI%s\n' 'BAN' > "$fixture/docs/notes.md"
printf 'url = "github:scorsi/ki%s";\n' 'so' > "$fixture/flake.nix"
printf '%s\n' 'example.org; the deployment repository; a TLS-terminating reverse proxy' > "$fixture/comment"
bash "$check" "$fixture"
echo "PASS: product wording, root docs and GitHub flake input"

printf 'url = "github:scorsi/ki%s"; # ji%s\n' 'so' 'ban' > "$fixture/flake.nix"
expect_rejected 'deployment wording alongside allowed input'
rm "$fixture/flake.nix"
printf 'github:scorsi/ki%s\n' 'so' > "$fixture/comment"
expect_rejected 'GitHub reference outside flake inputs'
rm "$fixture/comment"
printf 'JI%s\n' 'BAN' > "$fixture/nested/docs/notes.md"
expect_rejected 'uppercase wording in nested docs'
