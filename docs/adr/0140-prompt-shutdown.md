# ADR 0140 — Prompt shutdown: long-lived streams end, the drain is bounded

Status: Accepted — 2026-10-31. Amends 0022.

## Context

On `SIGTERM` or `SIGINT` the server logged `stopping`, closed its listeners and waited for every accepted connection to finish (0022). A runner's `Connect` stream (0105) and a following `WatchRun` (0120, 0129) never finish on their own, so a stop waited for systemd's stop timeout and its `SIGKILL`: a restart of `kajiba-server` took more than a minute whenever a runner or a follower was connected. The owner decided on 2026-10-31 that a stop must be prompt.

## Decision

- **Long-lived streams end at once.** When the server stops, every `Connect` stream and every following `WatchRun` ends with `UNAVAILABLE` ("the server is stopping"). Their clients already resume from it: the runner reconnects with its backoff (0105) and its in-flight attempts continue (0008); the CLI resumes the log from its offset (0130).
- **The drain is bounded.** Other in-flight requests (unary RPCs, git transport, `FetchRunBundle`, a worktree upload, the local and git-side sockets) get **5 s** to finish. Connections still open after it are closed with the process, after a `drain-timeout` warning; the process still exits 0.
- These are constants, not configuration.

## Consequences

- A restart takes a few seconds plus startup, whatever is connected.
- A clone, fetch or upload longer than the drain is cut by a stop; its client retries. An interrupted push is recovered by the startup reconciliation of `pending_push` (0003), as after any stop.
- Amends 0022: event `drain-timeout` (warn).

## Rejected alternatives

- Raising or lowering systemd's stop timeout only: the server would still wait for streams that never end, and `SIGKILL` skips the exit path.
- An unbounded drain for ordinary requests: one stalled client would hold every restart.
