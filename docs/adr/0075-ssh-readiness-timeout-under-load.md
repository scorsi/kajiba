# ADR 0075 — SSH readiness timeout under load

Status: Accepted — 2026-10-25

## Context

The J3 `nixos-git-ssh` check can receive a 503 from `/readyz` after the
service's startup readiness probe has passed. On a loaded VM, a healthy local
sshd can take more than ADR 0072's one-second budget to emit its banner. Once
the shared cache expires, that delay becomes a failed probe cached for three
seconds.

## Decision

- **Amends 0072 — SSH readiness timeout.** A single constant three-second
  timeout covers TCP connect and reading the first eight banner bytes. The
  required prefix remains `SSH-2.0-`; connection failure, timeout, short input
  or another prefix still means not ready.
- The three-second shared cache, concurrent-miss coalescing and cancellation
  behavior of 0072 remain unchanged. A previous success is not retained after
  a failed probe.

## Consequences

- A healthy sshd can take more than one second to answer without causing a
  readiness failure. A stalled probe can now delay HTTP or health RPC responses
  by three seconds, below the module's five-second startup curl deadline.
- A stalled sshd can take up to the remaining cache lifetime plus three seconds
  to be reported unavailable. Immediate connection failures still fail promptly
  after cache expiry.
- A Rust regression expires a successful cache entry, delays the next banner
  by 1.5 seconds and checks HTTP readiness plus the shared health RPC result.
  Existing tests retain invalid-banner, stall, recovery and cancellation checks.
- The SSH integration test waits for readiness at startup and after a service
  transition, as other integration tests do.

## Rejected alternatives

- Keeping the one-second timeout and only retrying the integration test: leaves
  product readiness sensitive to normal VM scheduling delays.
- Retaining a previous success after a timeout: adds stale-success policy and
  can hide a genuinely stalled sshd. A larger bounded timeout is sufficient for
  the observed delay.
- A configurable timeout: unnecessary configuration for a local probe.
