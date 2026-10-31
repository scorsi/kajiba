# ADR 0130 — The CLI resumes a run's log after any transport failure

Status: Accepted — 2026-10-31. Amends 0117.

## Context

ADR 0117 has `kajiba ci run` and `kajiba ci watch` resume a dropped log stream from the bytes received, but the CLI only resumed on `UNAVAILABLE`, `UNKNOWN`, `CANCELLED` and `DEADLINE_EXCEEDED`. Through a reverse proxy, a stream reset or a broken HTTP/2 connection surfaces as other codes (`h2 protocol error: error reading a body from connection` arrives as `INTERNAL`), and `kajiba ci watch` gave up on a run that was still going. The owner decided on 2026-10-31.

## Decision

- **Amends 0117 — what resumes.** Following resumes (same offset, 1 s delay, at most 30 times in a row without new bytes) after any failure that is not one of the forge's own answers:
  - every error the client's transport raises (stream reset, closed or reset connection, HTTP/2 protocol error), whatever gRPC code it maps to;
  - a status whose code is `UNAVAILABLE`, `UNKNOWN`, `CANCELLED`, `DEADLINE_EXCEEDED`, `ABORTED`, `INTERNAL` or `DATA_LOSS`, as a proxy or a failing forge answers;
  - a stream that ends without the terminal run.
- The forge's answers end the command with exit 1 as before: `INVALID_ARGUMENT`, `NOT_FOUND`, `ALREADY_EXISTS`, `PERMISSION_DENIED`, `RESOURCE_EXHAUSTED` (`WATCH_STREAMS_LIMIT`), `FAILED_PRECONDITION`, `OUT_OF_RANGE`, `UNIMPLEMENTED`, `UNAUTHENTICATED`. The terminal run ends it with the run's exit code (0117).
- `kajiba ci logs` does not follow and still ends on the first failure.

## Consequences

- A follower survives proxy resets and server restarts shorter than about 30 s without new bytes, printing nothing twice.
- An `INTERNAL` the forge itself returns (a storage error) is retried up to 30 times before the CLI reports it, about 30 s later than before.

## Rejected alternatives

- Listing the resumable codes, as before: every proxy or transport maps resets to its own code, and the list missed `INTERNAL`.
- Resuming on any code: a refusal such as `RUN_NOT_FOUND` would retry 30 times before reporting a definitive answer.
