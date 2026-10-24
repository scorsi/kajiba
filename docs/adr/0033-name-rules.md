# ADR 0033 — Name rules: reserved names and lowercase enforcement

Status: Accepted — 2026-10-24

## Context

J1 implemented the shared name validator and the `user` table (0013, 0021, `docs/schema.md`). Implementation showed that the list of reserved names and the way lowercase user names are enforced were left implicit. As README says, an accepted ADR is not edited: each item below names the ADR point it amends; everything else in that ADR stands. Each item describes what the J1 code does and is covered by its tests.

## Decision

- **Amends 0021 — reserved names.** The forbidden owner and repository names are every name starting with `kajiba.` (every gRPC package of the product: `kajiba.v1`, `kajiba.runner.v1`, `kajiba.admin.v1`, …) or `grpc.` (gRPC reflection and other `grpc.*` services), plus `raw`, `healthz`, `readyz`, `auth` and `metrics`, compared after lowercase normalization, in the shared validator of `kajiba-core`.
- **Amends 0013 and `docs/schema.md` — lowercase `user.name`.** Lowercase user names are enforced in the database by `BEFORE INSERT` and `BEFORE UPDATE OF name` triggers (migration 6) that abort with the same message as a CHECK. SQLite cannot add a CHECK to an existing table without rebuilding it, and the rebuild's `DROP TABLE` would cascade through the foreign keys, because migrations run in a transaction where foreign keys cannot be disabled.

## Consequences

- No owner or repository path can shadow a route or a gRPC service of the product.
- A future table change on `user` keeps the triggers or replaces them with a CHECK in a rebuilt table, with foreign keys handled explicitly.

## Rejected alternatives

- Rebuilding the `user` table to add a CHECK: cascades through foreign keys inside the migration transaction.
