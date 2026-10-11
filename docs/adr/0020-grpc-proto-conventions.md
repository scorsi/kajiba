# ADR 0020 — gRPC and proto conventions

Status: Accepted — 2026-10-11

## Context

The `.proto` files are the source of truth for the public API, the runner protocol and the internal hook API (0003, 0005, 0008). Three generated clients (Rust, Go, TypeScript).

## Decision

- **buf** tooling: `buf lint` and `buf generate`; `buf breaking` against the last published version, run in `nix flake check`.
- Packages: `kajiba.v1` (public API), `kajiba.runner.v1`, `kajiba.hook.v1`.
- **During v0** (before the first version deployed in production on ishizue), breaking changes are free; `buf breaking` is informational only. From the first production version on: additive only, otherwise a new `v2` package.
- Errors: standard gRPC codes; structured details through `google.rpc.Status` (`ErrorInfo`, `BadRequest`, `PreconditionFailure`). Never a bare text message as the contract.
- Every list is paginated by **opaque cursor** (`page_size`, `page_token`, `next_page_token`).
- Exposed identifiers are **opaque** (ULID as text); integer keys in the database, never exposed.
- Times as `google.protobuf.Timestamp`; durations as `google.protobuf.Duration`.
- Naming: singular resources in messages, `<Resource>Service` services, `Get/List/Create/Update/Delete` methods + explicit business verbs (`TriggerRun`, `CancelRun`).
- Browser security: grpc-web calls have a non-simple `Content-Type`, so every cross-origin call triggers a CORS preflight; CORS allows only the forge's origin; session cookie `HttpOnly; Secure; SameSite=Strict`. No plain HTTP route mutates state, except the WebAuthn ceremonies protected by their challenge.

## Consequences

- The CLI and the frontend contain no manual type definitions.
- A contract break after going to production is a major version change (0023).

## Rejected alternatives

- protoc + plugins by hand: less reproducible, no `breaking`.
- Offset pagination: inconsistent under concurrent writes.
