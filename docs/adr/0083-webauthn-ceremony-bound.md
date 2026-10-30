# ADR 0083 — Outstanding WebAuthn ceremony bound

Status: Accepted — 2026-10-30

## Context

ADR 0014 keeps WebAuthn ceremony challenges in memory for five minutes (docs/schema.md: lost on restart). The enrollment and login start routes are reachable without a credential, so without a bound any client could make the server hold an unlimited number of ceremony states. No ADR fixes that bound nor what happens when it is reached. The owner decided it on 2026-10-30.

## Decision

- **Amends 0014 — ceremony bound.** At most 64 unexpired ceremonies (enrollment and login together) are held in memory. A start beyond that is refused, the ceremonies in progress are kept, and the refusal is logged as `webauthn-ceremonies-full{limit}`. Expired ceremonies never count.
- A ceremony is named by a random id the start route returns; finish removes it, so an id is usable once, and an unknown, finished or expired id is refused.
- The relying party is built from `http.public_url`: RP ID = its host, origin = the URL with its port. A host that is not a domain name cannot be an RP ID, so the server refuses to start with one.

## Consequences

- A flood of start requests can delay legitimate ceremonies by at most five minutes, never grow memory.

## Rejected alternatives

- Evicting the oldest ceremony: a flood would silently break the ceremonies users are completing.
- A bound per client address: the forge sits behind a reverse proxy and has no trusted client address.
