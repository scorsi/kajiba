# ADR 0091 — CLI dependencies already in the workspace

Status: Accepted — 2026-10-30

## Context

ADR 0076 lists the J4 dependencies of `kajiba-cli` that were new to the workspace. The CLI also uses crates that are already direct dependencies of other kajiba crates, or transitive ones; the owner decided on 2026-10-30 that such a crate may become a direct dependency whenever it simplifies the code, provided the dependency register (0034) lists it.

## Decision

- **Amends 0076 — `kajiba-cli` dependencies from the workspace.**

  | Crate         | Used by      | Why                                                                                             |
  | ------------- | ------------ | ----------------------------------------------------------------------------------------------- |
  | `tonic-types` | `kajiba-cli` | reading the reason and request id of a refusal (`ErrorInfo`, 0020) for the CLI's error messages |
  | `libc`        | `kajiba-cli` | turning terminal echo off while `kajiba login` reads a token (0089)                             |
  | `prost`       | `kajiba-cli` | encoding a message to re-read it as a dynamic message for `--json`                              |
  | `serde_json`  | `kajiba-cli` | writing the protojson of `--json` (`prost-reflect`'s `serde` feature, 0076)                     |
  | `tempfile`    | `kajiba-cli` | exclusive temporary files and cleanup for atomic credential-file replacement (0089)             |
  | `kajiba-core` | `kajiba-cli` | shared strict semantic version parsing for request and response headers (0080, 0090)            |

## Consequences

- No new crate enters `Cargo.lock`.

## Rejected alternatives

- Hand-decoding `google.rpc.Status` or calling the terminal through a shell `stty`: more code for what the workspace already provides.
