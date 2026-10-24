# ADR 0035 — J2 direct dependencies

Status: Accepted — 2026-10-24

## Context

J2 implements `GitStore` reads, repository RPCs, import and smart HTTP. ADR 0034 requires each milestone's direct dependencies, their users and their reasons to be recorded in a new ADR of this theme.

## Decision

- **Amends 0034 — J2 dependencies.**

  | Crate                                                       | Used by         | Why                                                                 |
  | ----------------------------------------------------------- | --------------- | ------------------------------------------------------------------- |
  | `gix` 0.90 (`sha1`, `blob-diff`; default features disabled) | `kajiba-git`    | `GitStore` reads and tree diffs, as allowed by 0003                 |
  | `base64` 0.22                                               | `kajiba-server` | opaque list cursors (0020); already transitive via sqlx/tonic       |
  | `zlib-rs` 0.6                                               | `kajiba-server` | inflating gzipped smart HTTP requests; already transitive via gix   |
  | `tokio-util` 0.7 (`io`)                                     | `kajiba-server` | streaming git output into HTTP responses; already transitive via h2 |

- **Amends 0034 — existing dependencies.** `tokio` gains `process` and `io-util` for spawning git and writing its stdin. `serde_json` 1, already in the register, moves from a dev-dependency to a normal dependency of `kajiba-server` for pending-push updates and durable event payloads.

## Consequences

- J2's dependency additions and feature changes are reviewable together; Git operations remain behind `GitStore` (0003).
