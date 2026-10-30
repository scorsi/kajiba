# ADR 0100 — WebAuthn request admission

Status: Accepted — 2026-10-30. Amends 0083.

## Context

ADR 0083 bounds the ceremonies held in memory, not the requests that create or finish them. The `/auth/webauthn/*` routes are reachable without a credential, and their JSON bodies were buffered with the framework's default 2 MiB limit before any check: with the connection allowance of the public listener, unauthenticated clients holding nearly complete uploads could make the server buffer about 2 GiB. The owner decided on 2026-10-30 to bound these requests as 0057 and 0058 bound repository reads.

## Decision

- **Amends 0083 — request size.** A request body to `/auth/webauthn/*` is at most **64 KiB**; a larger one is refused with HTTP `413` before reaching a handler. A body exactly at the limit proceeds. This is a constant, not a configuration key.
- **Amends 0083 — admission.** The four routes share a budget of **32** in-flight requests. A request takes one permit **before its body is read** and keeps it until its handler answers. Without a free permit it is refused at once, without reading the body or queuing: `503`, `Retry-After: 1`, the existing refusal shape `{"reason": "BUSY"}`, logged as `webauthn-refused{reason}`.
- `BUSY` (requests in flight) is distinct from `CEREMONIES_FULL` (outstanding ceremonies, 0083): the first clears as soon as a request ends, the second after a ceremony finishes or expires.

## Consequences

- Ceremony bodies buffered at once are bounded by 32 × 64 KiB = 2 MiB. A real attestation or assertion is a few KiB.
- More than 32 simultaneous ceremony steps get `503` and retry; the web application shows the reason in words.

## Rejected alternatives

- Reusing the public read budget of 0040/0058: a burst of sign-in attempts would refuse anonymous Git reads, and the reverse.
- Reusing `CEREMONIES_FULL`: it names another bound, with another recovery time.
- Queuing beyond the budget: waiting requests would hold their connections, the resource this bounds.
