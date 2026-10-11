# ADR 0012 — Storage: SQLite, on-disk layout, impermanence

Status: Accepted — 2026-10-09

## Context

One machine, one process. Host with impermanence (root wiped at boot). PostgreSQL conceivable later only.

## Decision

- **SQLite** in WAL mode, a single writer, access through sqlx, **versioned SQL migrations** embedded in the binary and applied at startup.
- Portability to PostgreSQL kept open: standard SQL, no JSON1 or exotic types in the schema, no logic in triggers.
- **Job queues** (CI, mirror, reconciliation) live in the DB; no broker.
- Layout under `stateDir` (default `/var/lib/kajiba`): `repos/<owner>/<name>.git`, `db/kajiba.sqlite`, `ci/logs/`, `ci/artifacts/`, `tmp/`. The NixOS module exposes `stateDir` and the list of sub-paths to persist, to be mapped into `environment.persistence` on the host side.
- Secrets (GitHub tokens, outgoing keys, possible signing key): files provided by sops, paths passed through module options; never in the DB.
- CI logs written in chunks on disk, index in the DB; retention and cleanup by timer.

## Consequences

- Consistent backup through the SQLite backup API (0016); never a raw copy of a file in WAL mode.
- Changing the DBMS is a data migration, not a code rewrite.

## Rejected alternatives

- PostgreSQL from the start: heavier in the VM, no benefit at this scale.
- External broker (NATS, Redis): useless on a single machine.
