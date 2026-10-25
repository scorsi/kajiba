# ADR 0068 — Shared request middleware dependencies

Status: Accepted — 2026-10-25

## Context

Quality review item 9 found that the git daemon lacked the server's request
correlation and error sanitization middleware (0020, 0022, 0031). Sharing that
middleware in `kajiba-core` changes the dependency users recorded by 0034.

## Decision

- **Amends 0034 — shared transport dependencies.** `axum` 0.7, `http-body` 1
  and `prost` 0.13 become direct dependencies of `kajiba-core` for the shared
  request middleware, streamed response trailers and structured error encoding.
  All three are already workspace dependencies in `Cargo.lock`.
- `tonic-types` 0.12, also already in the workspace and lock file, becomes a
  dev-dependency of `kajiba-git` to verify error details at the daemon RPC boundary.
- The server keeps its route classification and metrics; both daemons use the
  shared correlation, request logging and error sanitization implementation.
  The contracts of 0020, 0022, 0030 and 0031 are unchanged.

## Consequences

- Header and trailer errors follow the same policy on both daemons, with no
  new external dependency or version change.

## Rejected alternatives

- Copying the middleware into the git daemon: two implementations of the same
  error and logging contract would drift.
