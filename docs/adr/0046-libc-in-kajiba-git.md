# ADR 0046 — libc in kajiba-git

Status: Accepted — 2026-10-25

## Context

ADR 0041 bounds every git child of an import by a deadline, and 0044 keeps that bound when a descendant (such as an `ssh` transport) holds the child's pipes. Enforcing it needs process groups and exit supervision that the standard library does not expose.

## Decision

- **Amends 0034 as amended by 0035, 0042 and 0045 — `libc` in `kajiba-git`.**

  | Crate      | Used by      | Why                                                                                                                                           |
  | ---------- | ------------ | --------------------------------------------------------------------------------------------------------------------------------------------- |
  | `libc` 0.2 | `kajiba-git` | each import child runs in its own process group; exit is observed with `waitid(WNOWAIT)` and the whole group is killed at the deadline (0041) |

## Consequences

- The dependency register is 0034 as amended by 0035, 0042, 0045 and this ADR. `libc` was already in the lock file through tokio and `kajiba-core`.
