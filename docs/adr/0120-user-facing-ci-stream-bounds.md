# ADR 0120 — User-facing CI stream bounds

Status: Accepted — 2026-10-31. Amends 0097.

## Context

`WatchRun` (J6) keeps a server task per stream, re-reading the run every second and on every CI progress signal, and buffers log bytes for a reader that may never read them; any caller allowed to read a run may open as many as it likes. `CreateWorktreeRun` (0097) is client-streaming: its response head only exists once the whole bundle is read, so the 30 s request timeout bounded the entire upload (64 MiB needed more than 2 MB/s) while nothing else bounded a slow one. The owner decided on 2026-10-31.

## Decision

Product constants, not configuration:

| Point                       | Value                                                                                                  |
| --------------------------- | ------------------------------------------------------------------------------------------------------ |
| `WatchRun` streams          | at most 16 open per principal and 256 server-wide                                                      |
| Stalled `WatchRun` delivery | a stream whose next message cannot be handed to the transport for 60 s is cut                          |
| `CreateWorktreeRun` upload  | exempt from the 30 s request timeout; cut after 30 s without receiving a byte, and after 10 min in all |

- A `WatchRun` beyond either bound is refused with `RESOURCE_EXHAUSTED`, reason `WATCH_STREAMS_LIMIT`. A stream holds its place until it ends: the run is terminal, the caller closes it, or it is cut. A cut stream releases its task and place; the client may reconnect from the offset it reached.
- An upload cut by either bound ends with `DEADLINE_EXCEEDED`; nothing of it is kept (0097's refused bundle).

## Consequences

- A reader cannot hold more than 16 server tasks and their buffers through `WatchRun`, nor all readers more than 256.
- A 64 MiB bundle needs about 110 KB/s on average instead of 2.2 MB/s; a stalled client frees its upload within 30 s.
- `kajiba ci` follows a run with one stream; a user running more than 16 follows at once is refused for the 17th.

## Rejected alternatives

- Carrying `WatchRun` through the browsing response permit (0050): its one-hour lifetime would cut a long run's follower.
- A per-message timeout on uploads: it would require a whole 16 KiB chunk every 30 s, stricter than "a byte".
- Configurable bounds: no deployment asked for one.
