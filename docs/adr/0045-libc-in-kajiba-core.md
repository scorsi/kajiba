# ADR 0045 — libc in kajiba-core

Status: Accepted — 2026-10-25

## Context

ADR 0034 requires direct dependencies, their consumers and their reasons to be recorded. The register, as amended by 0035 and 0042, omits `libc`, which configuration and secret-file validation already uses. This corrects 0042's statement that no other direct dependency was added by the J2 review fixes.

## Decision

- **Amends 0034 as amended by 0035 and 0042 — `libc` recorded.**

  | Crate      | Used by       | Why                                                                                                                      |
  | ---------- | ------------- | ------------------------------------------------------------------------------------------------------------------------ |
  | `libc` 0.2 | `kajiba-core` | `O_NONBLOCK` for opening configuration and secret files before regular-file validation, without blocking on FIFOs (0029) |

- `kajiba-core` also uses `libc::mkfifo` in its configuration tests to verify refusal of FIFOs for configuration and secret files. No other crate in `crates/*/Cargo.toml` declares a direct `libc` dependency.

## Consequences

- The dependency register is 0034 as amended by 0035, 0042 and this ADR. No dependency or runtime behavior changes; `libc` was already present transitively through tokio.

## Rejected alternatives

- Leaving `libc` unrecorded because it was already transitive: 0034 requires the direct consumer and its reason to be recorded.
