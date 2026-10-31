# ADR 0129 — `WatchRun` keepalive

Status: Accepted — 2026-10-31. Amends 0120.

## Context

A reverse proxy in front of the forge may cut an HTTP/2 stream that carries no byte for a while, commonly after 60 s. A `WatchRun` follower sends nothing while a build is quiet, so `kajiba ci watch` lost its stream during a long silent build step. An HTTP/2 PING keeps the connection alive but not the stream, and proxies do not forward it. The owner decided on 2026-10-31.

## Decision

- **Amends 0120 — keepalive message.** A following `WatchRun` stream on which nothing was sent for 20 s carries a `WatchRunResponse` whose `event` is `keepalive` (an empty `WatchKeepalive`), then again every 20 s of silence. Any log byte, section or the terminal run restarts the count. A one-shot read (`follow` false) never carries one.
- Clients ignore it; it moves no offset. A client built before this ADR decodes an unknown `oneof` member as no event and already ignores it.
- 20 s is a product constant: a third of the 60 s idle timeout common to reverse proxies, and of 0120's delivery stall bound, which the keepalive is subject to like any message.

Other long streams users reach through a proxy do not need one: `CreateWorktreeRun` is client-streaming (the client sends the bytes), and Git over HTTP carries Git's own progress keepalives. The runner channel and `FetchRunBundle` are on the runner listener (0124), not behind the HTTP proxy.

## Consequences

- A watcher costs one 7-byte frame every 20 s on a quiet run.
- A follower still drops on a real transport break; the CLI then resumes from its offset (0117).

## Rejected alternatives

- HTTP/2 PING: frames on the connection, not the stream; a proxy answers it itself and still times the stream out.
- Resending the last log byte or an empty `LogBytes`: an old client would treat it as log data and reset its retry count.
- A configurable interval: no deployment asked for one.
