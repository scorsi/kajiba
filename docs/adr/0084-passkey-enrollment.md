# ADR 0084 — Passkey enrollment

Status: Accepted — 2026-10-30

## Context

ADR 0014 enrolls a passkey through a single-use link printed by `kajiba-server admin enroll`, 0028 fixes the link's format and lifecycle, 0021 reserves `/auth/webauthn/*` for the ceremonies, and 0020 lets these ceremonies be the only state-changing plain HTTP routes. None fixes the WebAuthn user handle, the passkey's name, the shape of the ceremony routes, nor the page the link opens. The owner decided these points on 2026-10-30.

## Decision

- **Amends 0028 — link path.** The link is `<public_url>/-/enroll#<token>`: the web application's enrollment page; the token stays in the fragment.
- **Amends 0014 — enrollment ceremony.**
  - `POST /auth/webauthn/enroll/start` takes `{"token"}` (read by the page from the fragment) and answers `{"ceremony", "options"}`, the WebAuthn creation options. `POST /auth/webauthn/enroll/finish` takes `{"ceremony", "credential", "name"?}`, and on success answers `204` with the session cookie (0079).
  - Finish verifies the attestation, then in one transaction consumes the link with a guarded update, stores the passkey and opens a session: of two ceremonies started from the same link, one enrolls.
  - Refusals answer `{"reason"}` and are logged as `webauthn-refused{reason}`, never with a secret: `403` `ENROLLMENT_INVALID` (unknown, superseded, consumed or expired link, or disabled user, indistinguishably), `403` `CEREMONY_INVALID` (unknown, finished or expired ceremony), `403` `VERIFICATION_FAILED` (attestation refused, wrong origin included), `400` `NAME_INVALID`, `503` `CEREMONIES_FULL` (0083).
- **Amends 0014 — passkey identity.** The WebAuthn user handle is the 16 bytes of the user's public id (a ULID): stable, opaque, never the user name. The passkey's name is optional, at most 64 characters without control characters, empty when not given.

## Consequences

- A refused name does not consume the ceremony; any other failed finish does, and the user starts again from the same link while it is valid.
- The user handle reveals nothing a public id does not.

## Rejected alternatives

- A user handle derived from the user name: a renamed user would no longer match its passkeys, and the name would be stored by authenticators.
- A required passkey name: the enrollment page would need an extra step for a label the user can set later.
