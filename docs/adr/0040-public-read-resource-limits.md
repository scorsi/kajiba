# ADR 0040 — Public read resource limits

Status: Accepted — 2026-10-25

## Context

Anonymous smart HTTP and raw downloads share the server's process and memory budget. The per-request body limits of 0038 and the response-head timeout do not bound concurrent work, blob allocations or streaming response lifetimes.

## Decision

- **Amends 0005 and 0038 — anonymous read resource limits.** These limits are constants, not configuration keys.
- `/raw/` checks the Git object header before loading a blob and refuses sizes above **16 MiB** with HTTP **413**. A blob exactly at the limit is accepted.
- One server-wide semaphore with **32 permits**, shared by `info/refs`, `git-upload-pack` and `/raw/`, bounds concurrent work. Acquire before buffering requests, inflating gzip, spawning git or scheduling raw reads. Exhaustion returns HTTP **503** with `Retry-After: 1`, without queuing work.
- Raw reads retain their permit with the blocking work and the returned blob allocation, including when the handler is cancelled. Git responses retain their permit through process completion or cancellation; their output uses a bounded channel instead of accumulating the advertisement or pack.
- Streaming upload-pack responses, including advertisements, have a **60-second idle timeout** without bytes handed to the HTTP transport and a **one-hour absolute deadline**. Timers remain active when the consumer stops polling. Client cancellation or either bound kills and reaps the child; `kill_on_drop` provides fallback cleanup. Unsuccessful child completion fails the response body rather than reporting clean EOF.
- Multiple `Content-Encoding` fields or multiple coding values are refused with HTTP **415** before reading the request body. The supported single encodings remain absent, `gzip` and `x-gzip`; 0038's wire and inflated size limits remain unchanged.

## Consequences

- Large raw downloads must use Git instead. Busy clients retry; stalled or excessively long clones must reconnect.
- The shared budget bounds admitted work, not total repository complexity or the memory consumed internally by one git process. HTTP transport buffering may be ahead of socket delivery, but bounded output and independent timers prevent an unread response from pinning a child indefinitely.

## Rejected alternatives

- Relying on connection counts or reverse-proxy timeouts: HTTP/2 multiplexes requests and the application owns the child process.
- Configurable limits: unnecessary surface for J2; constants provide an explicit common policy.
- Loading arbitrary blobs or entire advertisements before responding: memory grows with repository contents.
