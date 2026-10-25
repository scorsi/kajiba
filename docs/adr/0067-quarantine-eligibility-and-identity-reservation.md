# ADR 0067 — Quarantine eligibility and repository identity reservation

Status: Accepted — 2026-10-25

## Context

J3 quality review item 1 shows that the RPC sequence `QuarantineRepository`,
`CreateRepository`, `Fetch` can replace a populated repository under the same
public id. Keeping the old directory in quarantine does not uphold ADR 0052's
promise that a compromised server cannot replace repository history.

The server owns the database and identifies orphan candidates. The git side
cannot independently verify that a candidate has no database row, and must not
trust that claim as authority to move stored history.

## Decision

- **Amends 0053 — `QuarantineRepository`.** A server-selected orphan candidate
  must independently be shown safe by the daemon: a valid repository with no
  refs, no stored objects and no active session. Only empty real `info/` and
  `pack/` directories may exist under `objects/`; other entries, symlinks and
  inspection errors fail closed. Ineligible repositories are left in place;
  refs, objects or an active session yield `FAILED_PRECONDITION`/`INVALID_STATE`.
  The mutation and session locks cover inspection through rename and fsync,
  so neither an import nor a newly registered SSH child can race the move.
- **Amends 0053 — `CreateRepository`.** An entry named `<id>.git` or starting
  with `<id>.git.` in the daemon's `tmp/orphans/` reserves that public id.
  Creation returns `ALREADY_EXISTS` even if `repos/<id>.git` is absent. This
  includes legacy numeric suffixes and entries of any type. The existing
  quarantine directories provide the durable reservation; startup cleanup
  retains them and the server has no RPC to remove them. Inspection errors
  fail closed. No new store or protocol field is needed.
- **Amends 0032 and 0052 — orphan handling.** The server still identifies
  candidates from its database, but automatic quarantine is restricted by the
  daemon checks above. Populated, malformed or otherwise unverifiably empty
  orphans remain under `repos/` and abort startup reconciliation for operator
  inspection. A privileged operator may recover or move them with the services
  stopped. Existing quarantines must be retained to reserve their identities;
  removing them is a privileged change outside the server compromise boundary.
  The server cannot quarantine populated history or recreate a quarantined
  identity through the RPC interface, preserving 0052's security promise.

## Consequences

- Empty repositories left by an aborted creation can still be quarantined,
  preserving their files, collision suffixes and parent-directory fsyncs.
- An orphan containing unreferenced objects, packs, alternates or object-store
  metadata needs operator inspection even when it has no refs. A failed import
  may leave such objects. Conservative refusal is preferable to moving history
  on an untrusted server's instruction.
- Repository identities are not recycled by RPC. A new repository uses a new
  public id; operator recovery must preserve the original history and identity.
- Tests cover the quarantine/create/import replacement sequence, stored objects
  without refs, active sessions, and identity reservation after daemon restart
  with a suffixed quarantine entry.

## Rejected alternatives

- Asking the server to confirm that the repository is unregistered: a
  compromised server controls the answer.
- Checking only directory absence on creation, or tracking quarantined ids
  only in memory: the replacement sequence works immediately or after restart.
- Allowing automatic quarantine of populated repositories while reserving the
  id: it would still let a compromised server remove active history from service.
