# ADR 0034 — Direct dependency register

Status: Accepted — 2026-10-24

## Context

ADR 0002 chooses Rust but gives kajiba no closed dependency list. J1 is the first milestone with code and added the first direct dependencies. As README says, an accepted ADR is not edited: this ADR amends 0002; everything else in it stands.

## Decision

- **Amends 0002 — dependencies are recorded.** Each direct dependency a milestone adds is recorded with the crate, its user and the reason, in a new ADR of this theme that amends this one. The direct dependencies added in J1:

  | Crate                                                             | Used by               | Why                                                                                                          |
  | ----------------------------------------------------------------- | --------------------- | ------------------------------------------------------------------------------------------------------------ |
  | `ssh-key` (`alloc`, `ecdsa`)                                      | `kajiba-core`         | OpenSSH public key parsing and SHA256 fingerprints (owner-approved); its `sha2` re-export also hashes tokens |
  | `serde`, `toml`                                                   | `kajiba-core`         | configuration (0019)                                                                                         |
  | `sqlx` (`sqlite`, `runtime-tokio`, `migrate`, `macros`)           | server                | database and embedded migrations (0012)                                                                      |
  | `tracing`, `tracing-subscriber` (`registry`, `std`), `serde_json` | `kajiba-core`, server | the log layer of 0022                                                                                        |
  | `http`                                                            | `kajiba-core`         | `public_url` parsing (`Uri`) in configuration validation                                                     |
  | `http-body`, `hyper-util` (`tokio`)                               | server                | already transitive: gRPC trailers in the request body wrapper, Unix-socket client for host-local commands    |
  | `tonic-types`                                                     | server                | `google.rpc.Status` and `ErrorInfo` details (0020, 0031)                                                     |
  | `tonic-reflection` (`server`)                                     | server                | gRPC reflection v1 (0005, 0031)                                                                              |

## Consequences

- The dependency set of each milestone is reviewable in one place per milestone, and its reasons outlive the commits that added it.
