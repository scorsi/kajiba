# ADR 0031 — gRPC error details and reflection

Status: Accepted — 2026-10-24

## Context

J1 implemented the gRPC transport (0005, 0020). Implementation showed points of error details and reflection that those ADRs left implicit. As README says, an accepted ADR is not edited: each item below names the ADR point it amends; everything else in that ADR stands. Each item describes what the J1 code does and is covered by its tests.

## Decision

- **Amends 0020 — error reasons.** The transport attaches an `ErrorInfo` with domain `kajiba` and `metadata.request_id` to every non-OK gRPC status. A non-`INTERNAL` error whose handler supplied no `ErrorInfo` gets an empty `reason`: only the handler knows the situation behind a code, so no reason is invented. `INTERNAL` keeps reason `INTERNAL`, the message `internal error`, and no other detail; the original message goes to the `request` log line only.
- **Amends 0005 and 0020 — reflection.** When `http.reflection` is enabled, only gRPC server reflection v1 (`grpc.reflection.v1.ServerReflection`) is served; v1alpha is not.

## Consequences

- Clients cannot rely on a reason for errors whose handler sets none; each handler adds its reason as RPCs arrive (J2 onward).
- Every error a client sees carries the `request_id` that finds its log line.

## Rejected alternatives

- Inventing a reason (such as `UNKNOWN`) for errors without handler details: a client could branch on a value no handler chose.
