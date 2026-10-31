# ADR 0113 — Execution backend direct dependencies

Status: Accepted — 2026-10-31. Amends 0034, 0103.

## Context

`kajiba-exec` (0009) gains its first code with J6: the `Executor` trait and the `nix` backend, which runs one process per step, streams its output and kills the step's process group on cancellation. Each crate below is a workspace dependency already approved for other crates.

## Decision

- **Amends 0034 and 0103 — `kajiba-exec` Rust dependencies.** Workspace versions and features apply.

  | Crate      | Used by       | Why                                                                         |
  | ---------- | ------------- | --------------------------------------------------------------------------- |
  | `tokio` 1  | `kajiba-exec` | already approved (0103): child processes and the log channel                |
  | `libc` 0.2 | `kajiba-exec` | already transitive through tokio: `kill` on the step's process group (0009) |

- `kajiba-runner` depends on `kajiba-exec` (a workspace crate).

## Consequences

- No new crate enters `Cargo.lock`.

## Rejected alternatives

- Killing only the step's leader process: children it started (Nix's helpers, a check's background process) would outlive the cancellation.
