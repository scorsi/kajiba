# ADR 0168 — Dump dependencies

Status: Accepted — 2026-11-01. Amends 0034.

## Decision

`kajiba-server` directly uses existing transitive dependencies `libsqlite3-sys`
0.30.1 (sqlx's SQLite backup API, through sqlx's locked connection handles) and
`sha2` 0.10 (streaming file hashes without loading packs into memory). No new
package is introduced. Keep the SQLite version aligned with sqlx's linked library.

## Consequences

Cargo.lock records these direct edges. No npm dependency changes; the pnpm deps
hash in nix/ should be checked if dependency packaging changes in a later task.
