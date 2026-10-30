# ADR 0081 — Token management from a session

Status: Accepted — 2026-10-30

## Context

ADR 0014 lets a user create personal access tokens (PATs) at runtime "from an authenticated session" and lists an `admin` scope; 0020 requires every method to declare a scope. Neither names the service, the scope its methods require, whether a PAT may manage PATs, nor what the `admin` scope grants a user who is not `is_admin`. The owner decided these points on 2026-10-30.

## Decision

- **Amends 0014 and 0020 — token management.**
  - Runtime PATs are created, listed and revoked through the `kajiba.v1` service `TokenService` (`CreateToken`, `ListTokens`, `RevokeToken`). Its methods declare the `admin` scope.
  - Only a browser session may call them. A PAT, whatever its scopes, is refused with `PERMISSION_DENIED`, reason `SESSION_REQUIRED`: a leaked token cannot mint or hide other tokens.
  - The `admin` scope never grants more than the user's real rights: for a user without `is_admin` it means admin on the repositories where the user's role is `admin`, nothing global.
  - `ListTokens` returns the caller's declared and runtime tokens, revoked ones included, ordered by name; `RevokeToken` on a revoked token changes nothing; a token of another user is `NOT_FOUND`, reason `TOKEN_NOT_FOUND`.

## Consequences

- Scripts and the CLI cannot rotate their own tokens; the user does it from the web UI.
- A revoked runtime token keeps its name (the `(user_id, name)` uniqueness of docs/schema.md) until the row is removed.

## Rejected alternatives

- Letting an `admin`-scoped PAT manage tokens: a stolen token would grant persistence.
- Treating the `admin` scope as global for every user: a scope would exceed the rights of its holder.
