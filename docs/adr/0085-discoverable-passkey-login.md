# ADR 0085 — Discoverable passkey login

Status: Accepted — 2026-10-30

## Context

ADR 0014 makes passkeys the only web login, and 0021 reserves `/auth/webauthn/*` for the ceremonies. No ADR says whether login first asks for a user name, which decides whether passkeys must be discoverable (resident) credentials. The owner decided on 2026-10-30 that login has no user name step.

## Decision

- **Amends 0014 — discoverable login.**
  - Enrollment requires a discoverable credential (`residentKey: "required"` in the creation options), so that the authenticator can offer it without a user name.
  - `POST /auth/webauthn/login/start` takes no input and answers `{"ceremony", "options"}`: request options with no allowed credential list and no mediation hint, the page choosing how to prompt. `POST /auth/webauthn/login/finish` takes `{"ceremony", "credential"}`; it finds the passkey by credential id, checks that the returned user handle is its user's (0084), verifies the assertion, then in one transaction stores the updated passkey state and `last_used_at` and opens a session, answering `204` with the session cookie (0079).
  - Refusals follow 0084: `403` `PASSKEY_INVALID` (unknown credential, passkey deleted by `admin enroll` or user removal, disabled user, user handle of another user, indistinguishably), `403` `CEREMONY_INVALID`, `403` `VERIFICATION_FAILED` (challenge, origin, signature or user verification refused), `503` `CEREMONIES_FULL`.

## Consequences

- Login is one step and reveals nothing about which user names exist.
- An authenticator that cannot store a discoverable credential cannot enroll.

## Rejected alternatives

- Username-first login: an extra step, and a way to probe user names.
