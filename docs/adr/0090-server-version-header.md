# ADR 0090 — Server version header

Status: Accepted — 2026-10-30

## Context

ADR 0023's server ↔ CLI row has the CLI print a warning, from 1.0, when the server's minor version differs from its own. ADR 0080 names the header carrying the client's version to the server, but nothing tells the CLI the server's version on a successful call.

## Decision

- **Amends 0080 — server version header.** Every `kajiba.v1` response, refusals included, carries the server version in the `kajiba-server-version` response header (gRPC initial metadata). The CLI reads it after each call and applies 0023's warning; it ignores a missing or malformed value.

## Consequences

- The warning needs no extra call.
- The server version is visible to any `kajiba.v1` caller, as it already is in the `VERSION_INCOMPATIBLE` message.

## Rejected alternatives

- A version field in `HealthService.Check`: one more call per command.
- No warning: 0023 requires it.
