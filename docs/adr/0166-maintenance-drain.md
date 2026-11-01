# ADR 0166 — Quiescent maintenance and runner persistence

Status: Accepted — 2026-11-01. Amends 0016, 0017, 0053, 0105, 0141, 0165.

## Context

Closing request admission alone does not stop background tasks or runner streams
from writing during a snapshot. Independent runner jobs must continue running.

## Decision

- Close admission, pause scheduler assignments and lease expiry, retention,
  session purging, declaration reload and ordinary event/reconciliation passes.
  Wait for admitted handlers and background writes without holding the database
  writer lock. Queued background work resumes after maintenance.
- Git exit reports, generation-local hooks and reconnect recovery remain usable
  while writing sessions drain. Refuse new PreReceive calls as in 0165. Once all
  writing sessions exit, freeze completion/reconnect writes too. Confirm the
  authoritative daemon's session inventory and generation before proceeding;
  an unavailable or changed snapshot fails the drain.
- Reconcile pending pushes and require none to remain, including retained rows.
  Flush every registered event handler to a fixed point, preserving transactional
  effects/checkpoints. A reconciliation or handler failure aborts maintenance;
  normal operation retains per-handler retry/backoff.
- Runner connections, execution messages and disconnect persistence wait behind
  admission. Acknowledgments follow persistence, never precede it. Drain admitted
  runner writes, never whole builds; independent job units keep running and the
  existing durable runner window supplies backpressure. On release, extend only
  leases still valid at barrier entry by the paused wall-clock duration before
  the next runner write or scheduler check; preserve heartbeat freshness too.
- Own both write barriers throughout snapshot/copy. Cancellation, success and
  failure release them by guard destruction, including cancellation during drain.
  Maintenance adds no shutdown wait; the existing server and Git shutdown bounds
  remain unchanged. No public protocol, config option or dependency is added.

## Consequences

J8 task 4 composes the internal drain guard with the verified payload. This ADR
adds no dump command or payload. Runner reconnects and acknowledgments can be
delayed by copying; running jobs are neither canceled nor awaited.
