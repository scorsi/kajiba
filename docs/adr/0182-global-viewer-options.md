# ADR 0182 — Global viewer options

Status: Accepted — 2026-11-01. Amends 0150, 0159.

## Context

The recovery checkbox only affects Needs attention and sits among page content.
The owner wants a global OPTIONS menu at the shell's top right and a filter that
works throughout the UI.

## Decision

- Add an icon disclosure named Options at the right of the shell, using the
  existing keyboard-accessible Popover. Its first preference is Hide fixed
  failures. Remove the old attention checkbox and stop using `fixed=1`.
- Default the preference on. Persist its boolean in localStorage under a key
  scoped to the signed-in viewer's name on this origin. Read and write inside
  try/catch; unavailable storage keeps an in-memory choice until reload.
- Apply the existing exact repository/ref recovery rule (0159) to failure
  lists everywhere: attention, Activity recent failures, repository Pipelines,
  shared History (including Administration), shell repository status badges
  and run-page history, plus Administration’s last-run links. Filter before display limits and use the full loaded
  snapshot as recovery evidence. Worktree failures remain visible.
- Toggling updates mounted views immediately without navigation or RPCs.
  Raw run data, aggregate execution statistics and an explicitly opened run
  remain unchanged; this preference filters lists, not historical outcomes.

## Consequences

Recovery remains bounded by loaded snapshots. The choice follows navigation
and reload in this browser, not other browsers or devices. No API, dependency,
configuration or NixOS changes. Node tests cover filtering and storage failure;
Playwright covers toggling across Activity, repository Pipelines and History,
reload persistence, and keyboard dismissal.
