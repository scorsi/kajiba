# ADR 0006 — Frontend: static SvelteKit embedded in the binary

Status: Accepted — 2026-10-09 (revised 2026-10-24: development origins accepted)

## Context

Preference for static (nothing at runtime on the host), JS required is acceptable, separate API. In development, a frontend change must not recompile Rust.

## Decision

- **SvelteKit** with `adapter-static` in SPA mode (`index.html` fallback), pnpm, TypeScript, connect-web client (0005).
- **Production**: the bundle is built by a Nix derivation (`packages.kajiba-ui`, `fetchPnpmDeps`) then **embedded in the server binary** (`rust-embed`/`include_dir`, bundle path provided at build time through an environment variable). A single artifact to deploy.
- **Development**: the Vite server (`pnpm dev`) serves the frontend with hot reload and **proxies grpc-web calls and `/auth/webauthn/*`** to a backend of choice, so the browser sees a single origin. No Rust recompilation for a frontend change. The Rust server also accepts `--ui-dir` to serve an external bundle without a rebuild (troubleshooting).
- **Development origins**: against a local server, the server runs with `http.public_url = "http://localhost:5173"` (the Vite origin; WebAuthn accepts `http` on `localhost`), so passkey login works as in production. Against a remote instance, WebAuthn cannot work (the relying party is the remote host): the Vite proxy injects `Authorization: Bearer` from the developer's PAT file (`KAJIBA_DEV_TOKEN_FILE`), a dev-only path that never exists in the bundle.
- No server-side rendering, no Node at runtime.

## Consequences

- The API must be complete for everything the UI displays; this is intended.
- No JS-free version: without consequence, non-browser uses go through the CLI, git smart HTTP and `/raw/`.
- In production, a frontend change recompiles the `kajiba-server` crate (dependencies stay in the crane cache).
- Playwright e2e tests against a real instance, from J4 (0015).

## Rejected alternatives

- SvelteKit SSR: permanent Node runtime, a second service.
- Astro + islands: suited to mostly static sites, not to an application.
- Server templates + htmx: the lightest, no-JS for free, but doesn't reuse Svelte.
- Directory served by the NixOS module in production: two artifacts to deploy for a marginal gain.
