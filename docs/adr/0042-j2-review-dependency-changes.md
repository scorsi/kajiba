# ADR 0042 — Dependency changes from the J2 review

Status: Accepted — 2026-10-25

## Context

ADR 0034 keeps a register of direct dependencies and requires each change to be recorded. ADR 0035 recorded J2's additions; the fixes that followed the J2 review changed one of them.

## Decision

- **Amends 0035 — `tokio-util` removed.** `tokio-util` 0.7 (`io`) is no longer a direct dependency of `kajiba-server`: the bounded upload-pack response stream (0040) is written by hand over tokio. It stays in the lock file as a transitive dependency of h2.
- No other direct dependency was added or removed by the J2 review fixes.

## Consequences

- The register is the 0034 list as amended by 0035 and this ADR.
