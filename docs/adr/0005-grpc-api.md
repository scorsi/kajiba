# ADR 0005 — API: gRPC, grpc-web for the browser, a single port

Status: Accepted — 2026-10-09

## Context

The API is a deliverable in its own right, consumed by the frontend (SPA) and the CLI (Go). Desire for lightness and strong typing.

## Decision

- The **`.proto`** files are the source of truth for the public contract and the runner protocol (0008). A single versioned package `kajiba.v1`.
- **tonic** server with **tonic-web** (grpc-web) for the browser; **connect-web** client on the SvelteKit side (grpc-web protocol), **grpc-go** on the CLI side.
- Streams: server-streaming for CI logs and status tracking. No bidirectional streaming on the public API (unsupported by browsers).
- **A single HTTP port** (axum router): gRPC/grpc-web, read-only git smart HTTP, `/raw/<repo>/<ref>/<path>` (for `fetchurl`), frontend assets, `/healthz`, WebAuthn ceremonies in JSON (the libraries speak JSON).
- Call auth: session cookie (browser) or bearer token (CLI, 0014), checked by a single interceptor.

## Consequences

- No REST: debugging with `grpcurl`; gRPC reflection is enabled outside production.
- TS and Go clients are generated; no manual type duplication.
- TLS: terminated by the existing reverse proxy (Tailscale certificates), the forge listens in plaintext on the host.

## Rejected alternatives

- REST + OpenAPI: more curl-able, but two schemas to maintain (API + runner) and less typing.
- GraphQL: nothing to gain.
