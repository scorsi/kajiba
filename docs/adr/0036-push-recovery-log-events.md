# ADR 0036 — Push recovery log events

Status: Accepted — 2026-10-24

## Context

J2's pending-push reconciliation emits two events absent from the log lists in 0022, 0030, 0026 and 0028. An audit of every `event = "…"` literal in `apps/` and `crates/` also finds `repo-orphaned`, already documented by 0032.

## Decision

- **Amends 0030 and 0022 — events added to the list.** The fields below are those emitted by the code, in addition to the common log envelope and any inherited request id (0022):
  - `push-retained{repo,ref,correlation_id}` (error): reconciliation found a ref value matching neither the recorded old nor new id. `repo` is `<owner>/<name>`, `ref` is the first unexpected ref, and `correlation_id` is the pending row's correlation id. No `err` field is emitted. The row is retained, further ref mutations stay blocked, and reconciliation continues with other rows.
  - `push-recovery-failed{err}` (error): periodic reconciliation returned an error. `err` is the returned error string; the next recovery pass retries.
  - `repo-orphaned{path,destination}` (warn), confirming 0032: startup moved an unregistered repository directory to quarantine. `path` and `destination` are the displayed source and destination paths; `msg` is `moved unregistered repository to quarantine`.
- The other unlisted literals are test fixtures only: `body-polled`, `handling`, `handled` and `in-flight` (info) test request correlation; `bad name` tests rejection of malformed event names and is never emitted by the log layer. They add no operational events.

## Consequences

- Operators can distinguish retained evidence from a failed recovery pass. The operational event register gains `push-retained`, `push-recovery-failed` and the cross-reference to `repo-orphaned`; all other audited operational literals are already listed.
