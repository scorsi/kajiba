# ADR 0159 — Active runs and recovered failures in Activity

Status: Accepted — 2026-11-01. Amends 0145 and 0151.

## Context

The owner's J5.1-b B4 feedback asks to see runs just started and to distinguish
failures followed by success from failures still needing attention.

## Decision

- Shared History includes queued and running runs alongside failures. Active
  events use creation time, without adding elapsed duration. Existing period
  and repository/path filters still apply. Activity Pipelines includes queued
  runs as well as running runs, with their actual status badges.
- A failed non-worktree run is **fixed** if the loaded runs include a succeeded
  non-worktree run created strictly later on the exact same repository and ref.
  Compare timestamp seconds and nanoseconds, independently of input order or
  commit identity. Empty manual refs match other empty manual refs only; no
  branch is inferred for recovery. Missing timestamps cannot prove recovery.
  Worktree runs neither become fixed nor fix another run.
- History and Activity's recent failures label recovered failures “fixed”.
  Needs attention retains default-branch scope (including manual runs of its
  loaded tip). It selects the newest failure by creation time and hides it by
  default if fixed. Queued, running and cancelled runs never hide a failure.
  A subsequent failure needs attention again.
- The shared attention area has a “Hide fixed failures” checkbox, checked by
  default. Unchecking sets `fixed=1` in the current page query, preserving other
  parameters; checking removes it. With the filter off, the newest failure per
  default branch remains visible and explicitly labelled if fixed. Mirrors and
  offline runners are unaffected.
- Queued and running non-worktree runs are linked in the attention area under
  a distinct “In progress” label with their actual statuses. Worktree runs
  remain visible in History and Pipelines but never count toward attention.

## Consequences

This remains a bounded browser composition: recovery can only be proven from
loaded recent runs (0145/0154). There is no new RPC, dependency or NixOS test.
Node fixtures cover event inclusion, ordering, recovery and worktree exclusion;
the existing Activity e2e covers an active run and the recovery filter.
