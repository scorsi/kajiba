# ADR 0127 — Runner connector dependency

Status: Accepted — 2026-10-31. Amends 0034, 0123.

## Context

The runner opens its TLS sessions itself (0124): tonic's channel takes a custom connector, and the connector's stream must implement hyper's I/O traits, which a `tokio-rustls` stream does not. `hyper-util` provides the adapter (`TokioIo`); it is already transitive through tonic and direct in the server (0034). The owner allowed on 2026-10-30 that a crate already transitive becomes direct without asking, recorded in the register.

## Decision

- **Amends 0034 and 0123 — `hyper-util` (`tokio`) is direct in `kajiba-runner`.** It adapts the runner's TLS stream to the channel's connector. The workspace version and features apply.

## Consequences

- No new crate enters `Cargo.lock`.

## Rejected alternatives

- Writing the I/O adapter in the runner: a copy of the one the crate already in the build provides.
