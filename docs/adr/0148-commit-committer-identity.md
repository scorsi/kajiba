# ADR 0148 — Commit committer identity

Status: Accepted — 2026-11-01. Amends 0051.

## Context

The repository commit detail needs to distinguish the author from the committer.
Browsing already returns the committer's timestamp but only the author's name
and email.

## Decision

- Add `Commit.committer_name` (field 7) and `Commit.committer_email` (field 8).
  Both `ListCommits` and `GetCommitDiff` return the identities stored in Git;
  neither identity is inferred from a forge account or the other identity.
- Amends 0051: both names and both emails count toward the existing history
  page byte budget, along with message and parent ids, on both history walks.
  Object, traversal, page and cursor limits otherwise remain unchanged.

## Consequences

Additive wire change; regenerate the committed TypeScript client. No new RPC,
permission, dependency or signature-verification claim.
