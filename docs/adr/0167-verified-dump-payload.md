# ADR 0167 — Verified dump payload

Status: Accepted — 2026-11-01. Amends 0016, 0032, 0052, 0067, 0069, 0097, 0122.

## Context

J8 needs a portable, independently verifiable payload spanning the two state
directories without giving the server write access to the Git state.

## Decision

- A dump is a directory named `dump-<Unix milliseconds>-<ULID>`. Build it in
  `.partial-<same suffix>` beneath `dump.dir`; publish by rename only after
  verification and file/directory fsyncs. Errors remove only this staging tree.
- `manifest.json` has format version 1, product version, creation time in Unix
  milliseconds, migration version, registered repository ids, and every payload
  directory and file. Paths are base64url without padding of relative Unix path
  bytes, preserving non-UTF-8 quarantine names. Files carry size and SHA-256.
  The manifest itself is the inventory, not a self-hashed payload entry.
- `server/db/kajiba.sqlite` is made with SQLite's backup API, never by copying
  the live database or WAL. `git/repos/` contains exactly the registered bare
  repositories, including archived ones. Preserve both `server/tmp/orphans/`
  and `git/tmp/orphans/`, including collision suffixes and empty directories:
  quarantine entries reserve identities even when no database row exists.
- Copy only those durable roots. Within registered repositories exclude lock files (`*.lock`), sockets,
  repository temporary `objects/incoming-*` directories and `tmp_*` pack files.
  CI bundles, logs, artifacts, caches, scratch, deployment configuration and
  secret files are outside the payload. Do not change source ownership or modes.
  Symlinks and other special files fail closed rather than following paths or
  silently losing durable state; quarantines requiring such handling need
  operator inspection. Quarantine names are never filtered: even a name ending
  in `.lock` can reserve an identity. Repositories using external object alternates
  are refused.
- Before publication, compare the copied inventory/hashes with the sources,
  verify all manifest entries and reject extra or missing content, check SQLite
  integrity and foreign keys, schema metadata and registered repository inventory,
  and run read-only Git bare-repository and full object/connectivity checks.
  Validation never migrates the snapshot. Invalid relative paths and symlink
  traversal are refused. A future offline restore reuses this verifier.
- The caller must retain task 2's quiescent maintenance guard throughout snapshot,
  copy and publication. This task adds no command, timer or external-copy lock
  protocol; those belong to their ordered tasks.

## Consequences

A published directory contains verified durable state and no live WAL files.
SHA-256 detects corruption, not malicious replacement of both payload and manifest;
dumps contain credential hashes and must receive the same protection as state.
