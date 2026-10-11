# ADR 0016 — Backup: consistent dump + rrsync pull

Status: Accepted — 2026-10-09

## Context

Existing scheme for Forgejo: daily dump, read-only rrsync pull to jiban, 30-day retention.

## Decision

- `kajiba dump <dir>` subcommand: SQLite snapshot through the backup API (`.backup`), copy of the bare repos (`git bundle` or copy after `git gc`), metadata (version, date, schema). Result: a timestamped directory, or an archive.
- Daily systemd timer in the NixOS module; the rrsync pull from jiban picks up the dump directory, retention unchanged.
- CI logs and artifacts are not backed up (ephemeral by definition).
- Restoration documented and tested in a `nixosTest`: dump → new instance → identical clone.

## Consequences

- The backup is consistent by construction; no raw copy of an open SQLite file.

## Rejected alternatives

- Direct rsync copy of `stateDir`: risk of an inconsistent DB in WAL mode.
- Continuous replication (Litestream): oversized.
