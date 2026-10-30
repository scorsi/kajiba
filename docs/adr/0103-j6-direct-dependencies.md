# ADR 0103 — J6 direct dependencies

Status: Accepted — 2026-10-30. Amends 0034.

## Context

J6 adds the runner protocol (0008), its server side, the `kajiba-runner` daemon and CI streaming to the CLI. ADR 0034 requires each milestone's direct dependencies, their users and their reasons to be recorded in a new ADR of this theme. Every crate J6 needs is already in `Cargo.lock`, either as a direct dependency of another kajiba crate or as a transitive one; the owner decided on 2026-10-30 that such a crate may become direct whenever it simplifies the code, provided the register lists it.

## Decision

- **Amends 0034 — J6 Rust dependencies.** Workspace versions and features apply unless a feature is listed.

  | Crate                                   | Used by         | Why                                                                                                            |
  | --------------------------------------- | --------------- | -------------------------------------------------------------------------------------------------------------- |
  | `tokio-stream` 0.1                      | `kajiba-server` | already transitive through tonic: server-streaming responses (run logs, source bundles) and stream adapters    |
  | `tokio` 1                               | `kajiba-runner` | async runtime of the runner's connection, heartbeats, reconnection and job processes (0008, 0009)              |
  | `tonic` 0.12 feature `tls-native-roots` | `kajiba-runner` | the `kajiba.runner.v1` client over HTTPS, trusting the system certificate store, as the CLI does (0076)        |
  | `serde_json`                            | `kajiba-runner` | log lines (0022) and reading `nix config show --json` for the host requirements (0009); was dev-only before J6 |
  | `libc`                                  | `kajiba-runner` | process groups and signals for cancelling a running job's process tree (0008, 0009)                            |

- A crate already in `Cargo.lock` that a later J6 task makes direct is listed in that task's dependency ADR, amending this one. A crate not yet in `Cargo.lock` is not added without the owner.

## Consequences

- No new crate enters `Cargo.lock`; the manifests change in the tasks that first use each crate.

## Rejected alternatives

- Hand-written `Stream` implementations over `mpsc` receivers instead of `tokio-stream`: more code for what tonic already pulls in.
