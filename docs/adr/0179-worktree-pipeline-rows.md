# ADR 0179 — Compact worktree pipeline rows

Status: Accepted — 2026-11-01. Amends 0151.

## Context

The owner finds the wrapping, unbounded worktree pipeline list hard to read.

## Decision

Repository Pipelines shows one compact row per worktree run: worktree badge,
triggering user, short base (or unknown base), selected checks (or all checks),
shared run-state icon, duration and age. Keep its run actions alongside it.
Bound the list to 16 rem with keyboard-accessible internal scrolling; narrow
screens may scroll horizontally. The full worktree identity remains a tooltip.
Other worktree labels retain 0151's wording.

## Consequences

No API, dependency or configuration change. Recent runs remain bounded by the
existing repository snapshot, with no new pagination contract.
