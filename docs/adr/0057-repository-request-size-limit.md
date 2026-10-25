# ADR 0057 — Repository request size limit

Status: Accepted — 2026-10-25

## Context

The J3 security review, item 5, found that the public read budget starts after
gRPC decoding. ADR 0043 admits browsing work after request validation and
repository lookup; metadata RPCs do not acquire a browsing permit. Tonic's
default 4 MiB message limit therefore allows large anonymous request messages
and cursor allocations outside the shared budget of ADR 0040.

## Decision

- **Amends 0040 and 0043 — bound inbound repository messages.** Every
  `kajiba.v1.RepoService` request, including `GetRepo` and `ListRepos`, has a
  **16 KiB** Protobuf message limit, excluding the five-byte gRPC frame header.
  This is a constant, not a configuration key.
- Configure the limit on the generated Tonic service. Tonic checks the message
  length before reserving its declared payload size or decoding Protobuf fields;
  an oversized message is refused with gRPC **OUT_OF_RANGE**. A message exactly
  at the limit proceeds to normal validation. Request compression remains disabled.
- The bound covers the entire message, including names, revisions, paths and
  pagination tokens, before handler-side cursor decoding and database lookup.
  The admission point and response delivery rules of 0043 and 0050 remain unchanged.

## Consequences

- Repository requests above 16 KiB are refused, even if they previously fit
  Tonic's default limit. Repository requests carry selectors and cursors, not
  file contents; larger combined selectors must use Git.
- This bounds each request's decoding work, not aggregate HTTP/2 buffering,
  concurrent metadata work or metadata response sizes. It does not turn the
  browsing semaphore into a global request admission limit.
- Transport tests cover all six repository RPCs with an oversized frame header
  alone, and prove the exact-size boundary reaches a handler while one extra
  byte is refused.

## Rejected alternatives

- Moving the shared permit ahead of decoding: requires propagating admission
  through requests, blocking work and responses. The message limit closes the
  large-request decoding gap with Tonic's existing mechanism.
- Separate field limits: redundant for bounding request allocations once the
  complete Protobuf message is capped.
