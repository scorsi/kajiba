# ADR 0037 — Push recovery period

Status: Accepted — 2026-10-24

## Context

ADR 0003 requires pending-push recovery at startup, on completion and on a timer, without fixing the timer's period. J2 implements it with the first ref mutation, import (0026).

## Decision

- **Amends 0003 — recovery period.** In `apps/server/src/db/pushes.rs`, `RECOVERY_INTERVAL` is `std::time::Duration::from_secs(60)`. Startup reconciles pending pushes after declared-state reconciliation and before serving. The periodic loop sleeps for 60 seconds before each pass, including the first; each subsequent delay starts after the previous pass finishes. Import also reconciles immediately after its mutating process exits.
- Recovery processes eligible rows in pending id order, skips repositories with a running admitted process, and excludes archived repositories until they are unarchived. Unexpected ref values retain the row and block admission (0003). A periodic error logs `push-recovery-failed` (0036) and the loop retries after the next delay; a startup reconciliation error aborts startup.

## Consequences

- Eligible pending pushes are retried without a new request or restart. The timer is a fixed 60-second delay between passes, not a deadline for completing recovery or permission to reconcile a still-running process.
