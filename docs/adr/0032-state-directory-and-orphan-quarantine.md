# ADR 0032 — State directory creation and orphan quarantine

Status: Accepted — 2026-10-24

## Context

J1 implemented the state directory and its startup cleanup (0012). Implementation showed that ADR 0012 left the directories' modes, their durability and the fate of a repository directory with no row implicit. As README says, an accepted ADR is not edited: each item below names the ADR point it amends; everything else in that ADR stands. Each item describes what the J1 code does and is covered by its tests.

## Decision

- **Amends 0012 — state directory creation.** `stateDir` and its sub-directories (`repos`, `db`, `ci/logs`, `ci/artifacts`, `tmp`, and `tmp/orphans` when needed) are created with the process umask and no explicit mode; the module's `UMask=0027` gives 0750. Each newly created directory and each orphan move is followed by an `fsync` of the affected parent directories.
- **Amends 0012 and 0022 — orphan quarantine.** At startup, after migrations and while holding the state lock, a directory under `repos/` that matches no registered repository is moved to `tmp/orphans/` (a numeric suffix `.1`, `.2`, … avoids overwriting an earlier quarantine) instead of being deleted, and `repo-orphaned{path,destination}` (warn) is logged; the event is added to 0022's list. Everything else under `tmp/` is cleared at startup; `tmp/orphans/` is kept across restarts.

## Consequences

- A stray repository directory is never lost: an operator inspects `tmp/orphans/` and removes or re-imports it.

## Rejected alternatives

- Deleting unregistered repository directories at startup: irreversible on a misconfiguration.
