# ADR 0175 — Current run status in snapshot lists

Status: Accepted — 2026-11-01. Amends 0138, 0144, 0145, 0150, 0159.

## Context

Repository cards and shell rows already use the same `RunBadge` as Pipelines,
Activity, Administration and the run page. The protocol and UI run enum values
agree. However, lists loaded while a run is queued retain that snapshot after
it starts. Activity's in-progress summary also always uses a running icon,
even when all its runs are queued, concealing the stale state.

## Decision

- Keep `RunBadge` as the single run-state-to-icon mapping: queued is a still
  grey icon, running a blue spinner, succeeded green, failed red, and cancelled
  retains its existing grey slashed circle. Unknown remains grey.
- Activity's in-progress summary uses running if any loaded run is running,
  otherwise queued. Individual badges always use the run state, never job or
  attempt state.
- Refresh Activity, repository Pipelines, Administration and the open shell
  overview one second after a successful snapshot containing queued or running
  runs. Stop when the displayed runs settle, the overview closes, the page is
  left or a request fails. Existing retry controls handle failures. Keep the
  previous data while refreshing and discard responses after cleanup.
- The run page keeps its existing WatchRun stream (0142). Lists use existing
  snapshot RPCs and introduce no additional streams.

## Consequences

Active snapshots incur repeated bounded RPC loads; idle lists do not poll for
new runs. No protocol, deployment configuration or dependency changes.
Node rendering tests cover every badge state; CI e2e loads both repository
lists while queued and asserts their blue spinners after the runner starts,
without reloading either list.

## Rejected alternatives

- A second icon mapping in repository rows: the shared badge already maps states
  correctly and cannot refresh its caller's snapshot.
- One watch per list entry: wastes the caller's bounded stream allowance (0120).
