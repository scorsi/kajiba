# ADR 0082 — Public logout

Status: Accepted — 2026-10-30

## Context

ADR 0077 makes every `kajiba.v1` method declare either a scope or the `public_access` marker, and names `HealthService.Check` as the public one. ADR 0079 makes logout the method `AuthService.Logout`, which deletes the caller's session row and clears the `kajiba_session` cookie. With a scope, the interceptor would refuse a call whose cookie names an expired or deleted session before the method runs, and the browser would keep a dead cookie it cannot clear itself (the cookie is `HttpOnly`).

## Decision

- **Amends 0077 — `AuthService.Logout` is public.** It declares `public_access = true`. It reads only the `kajiba_session` cookie (an `authorization` header is ignored), deletes the session row that cookie names if one exists, and always answers with the cookie cleared.

## Consequences

- An expired, unknown or already deleted session cookie is still cleared.
- Logout cannot be abused against another user: naming a session requires holding its secret, which is exactly what the session itself grants.

## Rejected alternatives

- A scope on `Logout`: a dead cookie would be refused and never cleared.
- Exempting `Logout` by name in the interceptor: the exception would be invisible in the contract, as 0077 already rejects for `Check`.
