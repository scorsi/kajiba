# ADR 0087 — Viewer method

Status: Accepted — 2026-10-30

## Context

The web application must know who is signed in, to show the user's name and to decide whether to show forge administration pages. The session cookie is `HttpOnly` (0079), so the page cannot read it, and no `kajiba.v1` method returns the caller. ADR 0077 requires every method to declare a scope or the public marker. The owner decided on 2026-10-30 that J4 adds such a method.

## Decision

- **Amends 0014 — `AuthService.GetViewer`.** It takes no input and answers `user`, the caller's user name, and `admin`, true when the user is a forge administrator (`is_admin`) and the credential holds the `admin` scope. A browser session holds every scope of its user, so for the web application `admin` is the user's `is_admin`; a PAT without the `admin` scope never reports one (0081).
- **Amends 0077 — its scope.** `GetViewer` declares the scope `repo:read`. An unauthenticated call is refused by the interceptor with `UNAUTHENTICATED` / `CREDENTIAL_INVALID` (or `CREDENTIAL_EXPIRED`), as for any scoped method.

## Consequences

- The web application learns that its session is gone from the same refusal as any other call.
- A PAT holding only `ci:trigger` or `repo:write` cannot call `GetViewer`.

## Rejected alternatives

- The public marker with its own credential lookup in the handler: a second resolution path beside the interceptor.
- Reporting `is_admin` whatever the credential's scopes: a PAT would advertise rights it cannot use.
