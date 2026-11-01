# ADR 0180 — Files navigation in a selected commit

Status: Accepted — 2026-11-01. Amends 0134, 0147.

## Context

The owner's Files feedback asks changed-file clicks to open a diff, ordinary
tree navigation to show content, and touched paths to remain visible in the tree.
The Files workspace can shrink within the viewport-bound repository scroll area,
leaving its nested content clipped or scrolling independently.

## Decision

- Amends 0134 and 0147: `view=diff` selects the file diff; absence selects content.
  Changed-file list links explicitly select Diff at the selected commit. Tree,
  spine, symlink and keyboard navigation select File, even for the same path.
  Path, ref and commit navigation clear the mode unless explicitly supplied.
  The File/Diff controls update this URL state; unrelated band toggles preserve it.
- During time travel, highlight exact touched paths and subtly mark their ancestor
  directories, including folded spines. Use the existing commit diff, with no extra
  browsing requests. Deleted paths remain in the changed-file list even when absent
  from the selected tree.
- The workspace has a 56 rem minimum (previously 28 rem), does not shrink to fill
  remaining viewport space, and grows with its preview. The repository scroll area
  owns vertical scrolling; the workspace retains horizontal scrolling for columns.
  No viewport-height subtraction or trailing filler is used.
- Diff lines reuse the file grammar and escaped HTML from the existing lazy syntax
  highlighter. Addition/deletion backgrounds remain on their rows. Unsupported
  languages remain plain; the 256 KiB highlighting cap also applies to diff lines.

## Consequences

No dependency, API, permission or NixOS test changes. Node tests cover mode
navigation, exact touched ancestors and highlighting; repository browser tests
cover click modes, tree marking and workspace growth.
