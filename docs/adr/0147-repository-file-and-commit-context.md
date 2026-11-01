# ADR 0147 — Repository file and commit context

Status: Accepted — 2026-11-01. Amends 0134, 0137.

## Context

The owner's J5.1-a feedback asks for content when opening a file, rendered
Markdown, a way back to repository history, and commit details and touched files
that remain reachable while browsing a historical tree. Selecting a commit must
not replace the list from which it was selected.

## Decision

- Opening a file defaults to its content; Markdown (`.md` and `.markdown`,
  case-insensitive) uses the existing sanitised renderer. Diff is an explicit
  toggle. A different path or revision resets the toggle to content.
- Path-filtered History offers a repository-history link that clears the path
  while keeping the current ref. History remains scoped to the current ref,
  including during time travel.
- The selected commit has a persistent section above Files: full message,
  full selectable id and copy button, author and committer identities, and exact
  local commit date and time (seconds included), with UTC on hover.
- The section lists `GetCommitDiff` changes independently of the preview. Every
  path links to that path at the selected commit; a deleted path says it was
  deleted. The file columns keep showing that commit's hierarchy.
- Amends 0134: `scope` preserves the History/Pulse path when entering time
  travel, including an explicitly empty scope for the repository. File navigation
  changes `path`, keeping `scope`, `ref`, `at`, and band/filter state. Back to now
  restores the preserved scope as the path. A `day` query selects Pulse's day
  list independently of `at`; selecting a commit alone keeps the existing list.
- Commit diff paths and the union in a run's What changed are sorted by path,
  using ascending case-sensitive string order, independently of browser locale.

## Consequences

No dependency or permission changes. Signature verification status is deferred:
no browsing RPC exposes it, and push admission does not establish the status of
imported history. The view keeps a TODO until a verification API is designed.
