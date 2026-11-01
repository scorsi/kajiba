# ADR 0165 — Maintenance admission and read-side credential usage

Status: Accepted — 2026-11-01. Amends 0016, 0053, 0078, 0079, 0083, 0118.

## Context

J8's maintenance barrier must refuse new mutations while browsing and cloning
remain available. Authentication of a read currently updates the credential's
last-used timestamp. WebAuthn ceremonies also create sessions or change passkeys.

## Decision

- Share one server-side admission barrier across public mutation handlers, local
  mutating commands, OpenSession for writing kinds, and PreReceive, including
  sessions opened before maintenance. Return UNAVAILABLE / MAINTENANCE with a
  retry message; the existing Git helpers print this message.
- Hold a mutation's admission permit through its completion or cancellation.
  Closing admission is immediate, even with admitted handlers still running;
  the maintenance owner reopens admission when its guard is dropped. Draining
  admitted writes and background work is the next J8 task.
- Reads, including SSH upload-pack, browsing and GetWorktreeBase preflight,
  remain available. Authentication still validates credentials and scopes but
  skips incidental PAT/session last-used writes when maintenance refuses their
  admission. Their timestamp is recorded on the next eligible use after release.
- All WebAuthn ceremony endpoints refuse HTTP 503 with JSON reason MAINTENANCE
  before consuming challenges or touching persistent state. This also prevents
  starting a ceremony that cannot finish during maintenance.
- Push completion (PostReceive and SessionExited) remains available. No protocol
  field, configuration option or dependency is added.

## Consequences

Read access cannot introduce credential usage writes behind a closed barrier.
A maintenance refusal leaves uploaded bundle storage, tokens, passkeys and local
admin state untouched. This task does not expose a dump command or claim quiescence.
