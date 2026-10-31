# ADR 0146 — The web application is served from a bundle directory, not embedded

Status: Accepted — 2026-10-31. Amends 0006.

## Context

0006 planned to embed the SvelteKit bundle in the `kajiba-server` binary. Since then `serve --ui-dir` (0094) serves an external bundle and the NixOS module wires it (0128); both paths are exercised by the end-to-end tests. Embedding would recompile the server for every interface change and add a second way to ship the same files.

## Decision

- **The bundle is never embedded.** `kajiba-server` serves the web application only from the directory given to `serve --ui-dir`; the NixOS module passes `packages.kajiba-ui` there. Without `--ui-dir`, page and asset paths answer as unknown routes. The sentence of 0006 that embeds the bundle (`rust-embed` or `include_dir`, environment variable at build time) is withdrawn; its other decisions stand.
- **Serving.** A precompressed `.br` or `.gz` sibling of a file is sent when the request's `Accept-Encoding` accepts it (`br` preferred), with `Content-Encoding` and `Vary: Accept-Encoding`. `_app/immutable/*` carries `Cache-Control: public, max-age=31536000, immutable`; every other file, `index.html` included, carries `no-cache`.
- **Budget.** A flake check fails when the gzip size of the initial JavaScript, CSS and fonts that `index.html` loads exceeds 300 KB.

## Consequences

- A frontend change does not recompile the server; the server crate stays free of any bundle path at build time.
- Running a server without a bundle is valid (API-only); the interface is then absent.
- Lazy route chunks are outside the budget; a route's own weight is a later decision.
