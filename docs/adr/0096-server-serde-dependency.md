# ADR 0096 — Server serde dependency

Status: Accepted — 2026-10-30

## Context

ADR 0034 records `serde` for `kajiba-core` configuration. J4 also uses it directly in `kajiba-server` to deserialize the JSON request bodies of the WebAuthn enrollment and login ceremonies (0084, 0085), but the J4 dependency register omits this user.

## Decision

- **Amends 0034 and 0076 — J4 server dependency already in the workspace.**

  | Crate                | Used by         | Why                                                                                    |
  | -------------------- | --------------- | -------------------------------------------------------------------------------------- |
  | `serde` 1 (`derive`) | `kajiba-server` | JSON request deserialization for WebAuthn enrollment and login ceremonies (0084, 0085) |

## Consequences

- The register covers the existing direct dependency; no manifest or `Cargo.lock` change is needed.
- Ceremony behavior is unchanged.

## Rejected alternatives

- Hand-decoding JSON request fields: more code than the workspace's existing deserialization derives.
