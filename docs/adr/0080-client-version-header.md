# ADR 0080 — Client version header

Status: Accepted — 2026-10-30

## Context

ADR 0023 has the CLI announce its version on every call and the server refuse an incompatible one with `FAILED_PRECONDITION` / `VERSION_INCOMPATIBLE` naming both versions (0020). It does not name the header, say whether a call without it is accepted, nor order the check against credential resolution (0014). The owner decided these points on 2026-10-30.

## Decision

- **Amends 0023 — client version header.**
  - The client sends its version in the `kajiba-client-version` request header on every `kajiba.v1` call.
  - A call without the header is accepted: browsers and generic gRPC tools (grpcurl) do not send it.
  - When present, the header is checked by the `kajiba.v1` interceptor before credential resolution, on every method including public ones. A value that is not `X.Y.Z` (optionally with a pre-release or build suffix) is incompatible.

## Consequences

- An incompatible CLI gets `VERSION_INCOMPATIBLE`, not a credential error, even with an expired or missing token.
- Clients that do not identify themselves are not version-checked; their compatibility is their own concern.

## Rejected alternatives

- Refusing calls without the header: the web UI and grpcurl would need to forge a version.
- Checking the version after authentication: an old CLI with a bad token would report the token, hiding the real problem.
