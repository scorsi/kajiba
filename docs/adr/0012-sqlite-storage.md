# ADR 0012 — Storage: SQLite, on-disk layout, ephemeral roots

Status: Accepted — 2026-10-09 (revised 2026-10-24: SQLite policy and repository layout accepted)

## Context

One machine, one process. The host may have an ephemeral root (wiped at boot, persisted paths declared). PostgreSQL conceivable later only.

## Decision

- **SQLite**, a single writer (the server process), access through sqlx, **versioned SQL migrations** (sqlx `migrate!`) embedded in the binary and applied at startup.
- **Connection policy**: `journal_mode=WAL`, `foreign_keys=ON`, `synchronous=FULL`, `busy_timeout=5000` ms on every connection; one write connection, a small read pool. **Single instance**: the server takes an exclusive `flock` on `<stateDir>/kajiba.lock` before opening the database and exits with `fatal{err="another instance holds <path>"}` otherwise. **Newer schema refused**: a database carrying a migration the binary does not know stops startup with an explicit error (no downgrade, 0023). A failed migration rolls back and stops startup.
- Portability to PostgreSQL kept open: standard SQL, no JSON1 or exotic types in the schema, no logic in triggers. JSON payloads (events, 0017) are opaque text. The event checkpoint scheme relies on the single writer (0017) and is revisited with any other DBMS.
- **Job queues** (CI, mirror, reconciliation) live in the DB; no broker.
- Layout under `stateDir` (default `/var/lib/kajiba`): `repos/<repo public id>.git`, `db/kajiba.sqlite`, `ci/logs/`, `ci/artifacts/`, `tmp/`, `kajiba.lock`. Dumps go to `dump.dir` (0016).
- **Repositories on disk are named by their immutable public id**, never by `<owner>/<name>`: a rename is a DB change only (0021). Creation: `git init --bare` in `tmp/`, `rename` into `repos/`, then commit the row; deletion: delete the row, then the directory. At startup, a directory in `repos/` without a row is moved to `tmp/orphans/` and logged; `tmp/` is otherwise emptied.
- The NixOS module exposes `stateDir` and the list of sub-paths a host with an ephemeral root must persist.
- Secrets (GitHub tokens, runner secrets, signing key): files provided by the deployment's secret manager, paths passed through module options; never in the DB or in the configuration (0019).
- CI logs written in chunks on disk, index in the DB; retention and cleanup by timer.
- Schema: [docs/schema.md](../schema.md) (0013).

## Consequences

- Consistent backup through the SQLite backup API under a write barrier (0016); never a raw copy of a file in WAL mode.
- Changing the DBMS is a data migration, not a code rewrite.
- `ls repos/` does not show names; `kajiba-server admin repo list` maps ids to names.

## Rejected alternatives

- PostgreSQL from the start: heavier, no benefit at this scale.
- External broker (NATS, Redis): useless on a single machine.
- `repos/<owner>/<name>.git`: readable, but a rename becomes a filesystem move to keep atomic with the DB, plus a recovery protocol for interrupted moves.
