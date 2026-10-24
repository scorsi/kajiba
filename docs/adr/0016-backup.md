# ADR 0016 — Backup: consistent dump under a write barrier

Status: Accepted — 2026-10-09 (revised 2026-10-24: write barrier, restore and retention accepted; off-host copy left to the deployment)

## Context

The forge's state is a SQLite database plus bare repositories; copied separately while pushes continue, they can disagree. Off-host copies depend on each deployment.

## Decision

- `kajiba-server dump` asks the running server (local socket, 0003) for a dump into `dump.dir` (default `/var/lib/kajiba-dumps`): a timestamped directory with the SQLite snapshot (backup API), a copy of every bare repository, and metadata (version, date, schema version).
- **Write barrier**: first close admission to pushes (`pre-receive`, explicit message, retry) and all other mutating operations (RPCs: `UNAVAILABLE`, reason `MAINTENANCE`, 0020; local commands, handlers and timers also stop admitting writes). Then drain all admitted mutations, including running `receive-pack` processes and their `pending_push` → event reconciliation (0003), handler transactions, CI writes and repository maintenance. Completion paths remain available while draining; a drain or reconciliation failure aborts the dump. Only after the drain completes take the SQLite snapshot and copy repositories, holding the barrier throughout both; release it when the dump finishes or fails. The snapshot and copies describe the same quiescent state. At this scale the snapshot and copy phase lasts seconds.
- **Restore**: `kajiba-server restore <dump-dir>`, offline, into an empty `stateDir` only (refuses otherwise); the server then starts normally (0023 applies: a dump from a newer schema is refused).
- Daily `kajiba-dump.timer` in the NixOS module; `dump.keep` (default 7) most recent dumps kept. Copying `dump.dir` off the host is the deployment's job (any pull or push tool reading a directory).
- CI logs and artifacts are not backed up (ephemeral by definition).
- Restoration tested in a `nixosTest`: dump → new instance → identical clone.

## Consequences

- The backup is consistent by construction; no raw copy of an open SQLite file.
- A push during the dump is refused once and replayed by the user.

## Rejected alternatives

- Direct rsync copy of `stateDir`: risk of an inconsistent DB in WAL mode.
- Continuous replication (Litestream): oversized.
- Dump without a barrier (DB first, then repositories): repositories may hold commits whose push events are missing from the snapshot.
