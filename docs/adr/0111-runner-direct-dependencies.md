# ADR 0111 — Runner daemon direct dependencies

Status: Accepted — 2026-10-31. Amends 0034, 0103.

## Context

ADR 0103 lists the `kajiba-runner` dependencies J6 needs as far as they were known. The daemon (J6 task 11) also serves `/metrics` (0022), writes its log lines through the `tracing` facade of the shared nisshi layer, and streams its side of `RunnerService.Connect`. Each crate below is already in `Cargo.lock`, transitive through tonic.

## Decision

- **Amends 0034 and 0103 — `kajiba-runner` Rust dependencies.** Workspace versions and features apply.

  | Crate              | Used by         | Why                                                                                 |
  | ------------------ | --------------- | ----------------------------------------------------------------------------------- |
  | `axum` 0.7         | `kajiba-runner` | already transitive through tonic: the loopback `/metrics` listener, as `kajiba-git` |
  | `tokio-stream` 0.1 | `kajiba-runner` | already transitive through tonic: the outbound half of the `Connect` stream         |
  | `tracing` 0.1      | `kajiba-runner` | already transitive through tonic: log events of the nisshi layer (0022)             |

## Consequences

- No new crate enters `Cargo.lock`.

## Rejected alternatives

- A hand-written HTTP responder for `/metrics`: more code for what axum already provides.
