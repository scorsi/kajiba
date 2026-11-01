# ADR 0183 — Restore invalidates non-terminal attempts

Status: Accepted — 2026-11-01. Amends 0170, 0008, 0108.

## Context

Dumps exclude log bytes. Restore clears their indexes, including sequence
acknowledgments, but a runner may still hold an unexpired attempt and have
already discarded acknowledged output. Continuing it with acknowledgment zero
would create sequence gaps and can stall execution indefinitely.

## Decision

- Offline restore installs a durable `db/restore-pending` marker in the server
  state directory alongside the restored database, under the same ownership,
  permissions and rollback rules. No sequence watermarks are preserved.
- On the first normal startup after restore, before declaration reconciliation or runner admission, mark every
  non-terminal attempt lost, regardless of lease expiry. Attempts are stored as
  `running` for both assigned and running jobs. Journal `attempt-lost` as system
  with the existing payload plus `reason: "restored"` (amends 0108).
- Apply the existing loss/replay rules: a replayable job below `max_attempts`
  becomes pending for a new numbered attempt; otherwise its run fails with
  reason `restored`. Terminal attempts and runs, and jobs still pending without
  an attempt, are unchanged. Reconnecting runners receive `CancelAttempt` for
  invalidated attempts through the existing resume rules.
- Commit all invalidations and their journal events in one transaction before
  removing and syncing the marker. A crash or failure retries reconciliation
  before admission; already lost attempts are unaffected. Ordinary restarts
  without the marker retain the existing lease and reconnection behavior.
- Missing worktree bundles are reconciled afterwards under 0097: an unfinished
  worktree run still fails `bundle-missing`; a run already failed at its attempt
  limit retains `restored`.

## Consequences

Restoring a dump may rerun checks even if the original runner kept executing.
Historical records remain, but execution cannot resume across discarded log
indexes. No migration, RPC, configuration option or dependency is added.
