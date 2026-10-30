# ADR 0079 — Session cookie and logout

Status: Accepted — 2026-10-30

## Context

ADR 0014 gives the web a session through an HttpOnly cookie, deleted by logout, and 0020 sets the cookie attributes and forbids state-changing plain HTTP routes outside the WebAuthn ceremonies. Neither names the cookie, the logout method, nor what happens to expired session rows. The owner decided these points on 2026-10-30.

## Decision

- **Amends 0014 and 0020 — session cookie and logout.**
  - The session cookie is named `kajiba_session`. The auth interceptor resolves it when a call carries no `authorization` header; a malformed `authorization` header is refused and never falls back to the cookie.
  - Logout is the `kajiba.v1` method `AuthService.Logout`: it deletes the caller's session row and clears the cookie.
  - Expired session rows are purged at startup and periodically; an expired row still present is refused with `CREDENTIAL_EXPIRED` (0020).

## Consequences

- A bearer token and a cookie on the same call: the token is what authenticates.
- The `session` table does not grow with abandoned sessions.

## Rejected alternatives

- A plain HTTP logout route: 0020 forbids state-changing plain HTTP routes outside the ceremonies.
- Keeping expired rows forever: unbounded growth for no audit value (sessions are not journal events).
