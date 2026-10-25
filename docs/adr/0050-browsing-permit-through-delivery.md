# ADR 0050 — Browsing permit through delivery

Status: Accepted — 2026-10-25

## Context

ADR 0043 put the browsing RPCs under the 0040 read budget and released their permit when the response was built; ADR 0047 bounded every browsing response to a few MiB and kept that early release on the ground that what outlives a permit is small. The final J2 verification found the ground insufficient: small individual responses do not bound their total. HTTP/2 multiplexes streams, so an anonymous client can keep many built-but-unread responses per connection, each holding its message in the transport's send path, while further calls reuse the released permits. The connection cap and the response-head timeout bound neither the number of outstanding bodies nor their lifetime, so the memory outside the budget grows with the client's patience, not with a server constant.

## Decision

- **Amends 0043 and 0047 — the permit travels with the encoded response.** A browsing handler leaves its permit in the response's extensions; a layer on the gRPC routes moves it, with the encoded body, onto a sending task of the kind `/raw/` uses (0043): the body's frames, data and trailers, are relayed under the 0040 stream bounds, the 60-second idle timeout without a frame handed to the HTTP transport and the one-hour absolute deadline. The task owns the body and the permit and drops both when the body ends or the first bound fires; an expired body ends in error, which resets the stream, not in a clean EOF. A refused call releases its permit at once, as before.
- **What outlives a permit.** Only the transport's own per-stream send buffer, which is bounded by the HTTP/2 implementation, holds copies after the task ends; the undelivered part of a message is freed with the task. The admitted memory of browsing is therefore at most 32 responses of the 0047 sizes at any time.
- The 1 MiB `GetBlob` cap and every other bound of 0047 remain: they bound each admitted response, and with them the budget.
- **Correction of 0047.** "The permit is still released when the response is built, as in 0043" no longer holds; the rejected alternative "carrying the permit through the encoded response" is the decision here, made possible by tonic keeping a response's extensions on the HTTP response and by the supervising task that 0043 introduced for `/raw/`. 0043's rejection of keeping the permit for as long as the transport lives still stands: the deadlines, not the connection, end it.

## Consequences

- A client that stops reading a browsing response loses it after 60 seconds and frees its permit; a reader slower than one hour loses it as well. Both get a reset stream.
- Tests assert that a browsing response holds its permit while unread through the router and releases it once the body is drained, and that a never-polled body expires and releases its permit.

## Rejected alternatives

- A global budget of outstanding response bytes: needs the same body supervision to know when bytes are delivered, and adds a second limit to explain.
- Bounding HTTP/2 concurrent streams per connection: bounds the count per connection, not the lifetime of what was built; it may complement this later as a transport setting.
- Streaming browsing responses: more API surface for results that a bound keeps small (0047).
