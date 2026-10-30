# ADR 0105 — Runner session: credential and protocol constants

Status: Accepted — 2026-10-30. Amends 0008.

## Context

ADR 0008 defines `kajiba.runner.v1` (one bidirectional stream per runner, declarative enrollment by shared secret, server-owned leases) and gives leases and heartbeats as defaults. It does not say how the secret travels, where the service sits beside the public API, what bounds log chunks, how a runner reconnects, nor what happens when two processes connect under one name. The owner decided these points on 2026-10-30.

## Decision

- **Amends 0008 — credential.** The runner's shared secret travels as `authorization: Bearer <secret>` metadata on every `RunnerService` call (`Connect` and `FetchRunBundle`); the runner's name travels in the first message of `Connect` (`Hello`) and is the declared name (0026). The server compares the secret in constant time. `RunnerService` is served on the public port (0005) outside the `kajiba.v1` layers: no scope option, no entry in the method access table, no gRPC reflection; it has its own authentication.
- **Amends 0008 — the two methods.** `Connect`, the bidirectional stream: runner → forge `Hello` (name, version, announced capabilities, hostname, in-flight attempt ids), heartbeat, acceptance, log chunk, step status, result; forge → runner assignment, cancel, continue, log acknowledgment, result acknowledgment. `FetchRunBundle`, server-streaming, carries an attempt's sources (0106). A refusal at admission ends `Connect` with a status (0020).
- **Amends 0008 — protocol constants.** These are product constants, not configuration:

  | Point                     | Value                                                              |
  | ------------------------- | ------------------------------------------------------------------ |
  | Lease                     | 30 s from the server's clock, renewed by each heartbeat            |
  | Heartbeat                 | every 10 s                                                         |
  | Jobs                      | replayable, `max_attempts` 3                                       |
  | Steps of a `nix` job      | one                                                                |
  | Log chunk                 | at most 64 KiB of log bytes; sequences start at 1 per attempt      |
  | Reconnection backoff      | 1 s, doubling up to 30 s; reset once a connection is admitted      |
  | Two connections, one name | the newer connection replaces the older one, which the server ends |

## Consequences

- A leaked secret lets its holder act as that runner only; rotating the file on both sides revokes it (0008).
- A restarted runner takes over its own name at once, without waiting for the old stream's lease to expire.
- Changing a constant is a code change and a protocol version decision (0023), not a deployment setting.

## Rejected alternatives

- The secret inside `Hello` only: `FetchRunBundle` would need its own copy, and a credential in a message body ends up in places metadata does not (message logs, debugging dumps).
- Configurable lease, heartbeat and backoff: no deployment asked for one; constants until one does.
- Refusing a second connection under a name already connected: a runner restarted before the server noticed the old stream's end would be locked out until the lease expires.
