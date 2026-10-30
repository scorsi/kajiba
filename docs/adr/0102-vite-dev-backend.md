# ADR 0102 — `pnpm dev` backend selection

Status: Accepted — 2026-10-30. Amends 0006.

## Context

ADR 0006 has the Vite development server proxy grpc-web calls and `/auth/webauthn/*` to "a backend of choice" and inject a PAT from `KAJIBA_DEV_TOKEN_FILE` for a remote instance. It does not say how the backend is chosen, nor how the PAT file relates to the CLI's credential file (0098). The proxy was not configured. The owner decided on 2026-10-30.

## Decision

- **Amends 0006 — backend.** `KAJIBA_DEV_URL` (default `http://127.0.0.1:8080`) is the backend `pnpm dev` proxies to: requests whose path starts with `/kajiba.v1.` (grpc-web) and `/auth/webauthn/`. Everything else stays with Vite. Without a token file, requests are forwarded as the browser sent them (local server with `http.public_url` set to the Vite origin, 0006).
- **Amends 0006 — PAT.** `KAJIBA_DEV_TOKEN_FILE` names a `kajiba login` credential file (0098). Its forge URL must equal `KAJIBA_DEV_URL`, and the URL must satisfy the CLI's transport rule (0099); otherwise `pnpm dev` refuses to start, naming the URLs. The proxy then sets `Authorization: Bearer` on every proxied request and drops the browser's `Origin` and `Cookie`: the Vite origin is foreign to the remote forge, whose origin check (0020) would refuse it.
- The proxy lives in `apps/ui/dev-proxy.js`, read only by Vite's development server; it is never in the bundle. Setup is documented in CONTRIBUTING.

## Consequences

- Against a remote instance, the UI acts with the PAT's scopes, not a session: session-only methods (0081) are refused there.
- The dev proxy has the CLI's guarantees: a PAT goes only to the forge it was saved for, never over plain HTTP to another machine.

## Rejected alternatives

- A bare token file, as 0006 first implied: a second token format beside the CLI's, without the URL that keeps it from reaching another forge.
- Rewriting `Origin` to the remote URL instead of dropping it: a forged browser origin, where an absent one is what a non-browser client sends.
