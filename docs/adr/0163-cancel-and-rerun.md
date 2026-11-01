# ADR 0163 — Cancel and re-run CI runs

Status: Accepted — 2026-11-01. Amends 0008, 0108, 0114, 0118 and 0122.

## Context

A user cannot stop a stuck run or repeat its checks from the CLI or run page.
The runner already accepts CancelAttempt and stops the whole transient job unit
(0156); the public RunService has no cancellation method.

## Decision

- `RunService.CancelRun(run)` and `RerunRun(run)` require `ci:trigger`, then a
  readable repository, not archived, the write role, and CI mode other than off,
  in the order of 0114/0118. Unknown runs give `NOT_FOUND` / `RUN_NOT_FOUND`;
  unreadable repositories give `REPO_NOT_FOUND`. Both return the resulting Run.
- Cancel ends a queued or running run, its unfinished job and running attempts in
  one writer transaction. A terminal run is returned unchanged: repeated cancel
  is successful, with no new events or changed timestamps. The first terminal
  result wins, including against a concurrent runner result. Late results are
  acknowledged and ignored under 0008.
- After commit, CancelAttempt is pushed to connected runners. The runner stops
  the transient unit and its cgroup (0156), or the process group on Darwin.
  Disconnected runners receive cancel during reconnection. A connection that
  falls behind the bounded cancellation channel is closed to force reconciliation.
  Cancellation records the terminal state immediately; it does not wait for the
  remote unit's stop timeout. WatchRun is woken and terminal metrics counted once.
- Cancellation journals `run-cancelled` `{run}` and, for each running attempt,
  `attempt-cancelled` `{run, attempt}`, version 1, with the cancelling user as
  principal, the run's repository and the request correlation id. The existing
  system `run-finished` `{run, state: "cancelled", check: null, reason: null}`
  remains in the same transaction, so existing consumers see every completion.
- Re-run creates a new queued run and pending job, never altering the original.
  It copies repo, SHA, ref, trigger kind, checks, base SHA and runner requirements;
  the caller becomes `triggered_by`. Its new optional `Run.rerun_of` and stored
  `run.rerun_of` contain the original public id. This text survives retention of
  the original run. `event_id` is empty: the new run is not another push event.
  The usual `run-created` event is journaled in the creation transaction.
- Re-run builds the previously admitted SHA even if refs have moved. A worktree
  re-run requires its original bundle still to exist, otherwise
  `FAILED_PRECONDITION` / `BUNDLE_MISSING` with an explanation. The retained bundle
  is copied and synced before commit, under the same writer lock as cleanup.
  The caller's active worktree limit includes this new run and concurrent uploads.
  Normal terminal-bundle cleanup still applies (0122); finished worktree runs
  therefore usually cannot be re-run. No new upload or retention setting is added.
- CLI: `kajiba ci cancel <run>` prints its id (or Run protoJSON) and exits zero
  on success. `kajiba ci rerun <run> [-d|--detach]` follows the new run with the
  same output and exit codes as `ci run`; detach prints the new id (or protoJSON).
- The run page offers Cancel for queued/running runs, with inline confirmation,
  and Re-run with a link back to the original on the new page. Pipelines lists
  offer inline-confirmed cancellation. Browser sessions have `ci:trigger`; actions
  are shown only for a viewer with write access, non-archived repo and enabled CI.
  Refusals, including expired worktree bundles, remain visible beside the actions.

## Consequences

No runner protocol, dependencies or configuration changes. Schema migration 17
adds a nullable public-id link without a foreign key, preserving it after retention.
The recovery VM test checks cancellation of a running transient unit and a linked
re-run; browser tests use the existing single-authenticator sign-in helper.

## Rejected alternatives

- Resetting the original run: loses the first result and its audit trail.
- Waiting for a runner result before recording cancellation: a disconnected or
  stuck runner would keep the run active and eligible for replay.
