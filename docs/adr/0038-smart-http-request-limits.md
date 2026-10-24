# ADR 0038 — Smart HTTP request limits

Status: Accepted — 2026-10-24

## Context

J2 implements read-only smart HTTP (0003, 0005). Upload-pack requests contain wants and haves, and git may gzip them; buffering and inflation need bounds.

## Decision

- **Amends 0005 — smart HTTP request bodies.** In `apps/server/src/transport/git_http.rs`, `REQUEST_LIMIT: usize = 16 << 20` is 16 MiB (16,777,216 bytes). `git-upload-pack` reads the body with `axum::body::to_bytes(body, REQUEST_LIMIT)` and returns HTTP 413 on failure, including a body above that limit. This limit applies to the compressed body too.
- `gzip` and `x-gzip` bodies are inflated with `zlib-rs`. Inflated bodies are strictly capped at 64 MiB (67,108,864 bytes, `REQUEST_LIMIT * 4`); bodies above that cap return HTTP 413. The output buffer starts at `min(body.len().max(1024) * 4, cap)` and grows with `min(size * 2, cap)`, never allocating beyond the cap. Malformed streams return HTTP 400 with `invalid gzip body`; unsupported content encodings return HTTP 415.

## Consequences

- Wire bodies are capped at 16 MiB and inflated bodies at 64 MiB. These limits describe smart HTTP upload-pack requests, not all HTTP or gRPC requests.
