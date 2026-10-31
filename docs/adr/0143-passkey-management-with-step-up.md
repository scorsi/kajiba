# ADR 0143 — Passkey management with a step-up

Status: Accepted — 2026-10-31. Amends 0014, 0083, 0084.

## Context

ADR 0014 lets a user add further passkeys "from an authenticated session" and makes `admin enroll` the recovery when every passkey is lost; 0084 fixes the enrollment ceremony and the user handle. None says whether an open session is enough to add or remove a passkey, whether a passkey can be removed at all, nor how the web application lists passkeys and the user's declared SSH keys. A stolen session cookie must not let its holder plant a passkey of their own or remove the owner's. The owner decided these points on 2026-10-31.

## Decision

- **Amends 0014 — step-up.** Adding or removing a passkey requires a **fresh WebAuthn assertion by one of the same user's passkeys** made from the same session, less than **five minutes** old. `POST /auth/webauthn/step-up/start` (session cookie, no body) answers `{"ceremony", "options"}`: request options naming the user's passkeys, user verification required. `POST /auth/webauthn/step-up/finish` takes `{"ceremony", "credential"}`; it verifies the assertion (challenge, origin, RP ID, allowed credential, signature, user verification), checks that the credential is a passkey of the session's user, records the passkey's use and stamps the session's `step_up_at`, answering `204`. The window belongs to that session only; it is not consumed by an addition or a removal.
- **Amends 0014 — additional passkeys.** `POST /auth/webauthn/register/start` (session cookie, step-up required) answers creation options with the user handle of 0084, a discoverable credential required (0085) and the user's passkeys excluded. `POST /auth/webauthn/register/finish` takes `{"ceremony", "credential", "name"?}` (name rules of 0084), checks again that the session is the one that started the ceremony and that its step-up is still fresh, and stores the passkey, answering `204` without a new cookie. Adding never replaces a passkey.
- **Removal.** `kajiba.v1` `PasskeyService.ListPasskeys` (the caller's passkeys, oldest first, paginated as 0020) and `DeletePasskey` declare the `admin` scope and, as `TokenService` (0081), refuse every PAT with `PERMISSION_DENIED` / `SESSION_REQUIRED`. `DeletePasskey` needs a fresh step-up of the calling session (`PERMISSION_DENIED` / `STEP_UP_REQUIRED`), refuses the user's last passkey (`FAILED_PRECONDITION` / `LAST_PASSKEY`) and answers `NOT_FOUND` / `PASSKEY_NOT_FOUND` for an id that is not one of the caller's passkeys. A removed passkey no longer signs in (0085's `PASSKEY_INVALID`).
- **Sessions stay valid** when a passkey is added or removed, including sessions opened with the removed one. The first passkey still comes from the enrollment link, and losing every passkey is still `admin enroll`, which deletes every passkey and session of the user (0014).
- **Amends 0083 and 0084 — ceremony routes.** The four new routes share the ceremony bound (64 outstanding ceremonies) and the request admission and body limit of 0100. A ceremony is bound to the session that started it: a finish carrying another session's cookie, even of the same user, is refused with `CEREMONY_INVALID` and consumes it. Their refusals follow 0084's shape: `401` `SESSION_INVALID` (no valid session cookie), `403` `STEP_UP_REQUIRED`, `403` `CEREMONY_INVALID`, `403` `VERIFICATION_FAILED` (a credential already registered included), `403` `PASSKEY_INVALID`, `400` `NAME_INVALID`, `503` `CEREMONIES_FULL` or `BUSY`.
- **Declared SSH keys are shown read-only.** `GetViewer` returns the caller's declared authentication and signing keys: usage, key type and `SHA256:` fingerprint, never the key itself; they change only in the configuration (0014).

## Consequences

- The `session` table gains `step_up_at` (docs/schema.md); the step-up survives a restart for the rest of its window, unlike ceremony state.
- Adding a passkey takes two prompts: one with an existing passkey, then the new one; the web application asks for each from its own click.
- A user who has lost every passkey but still holds a session cannot add one; recovery stays `admin enroll`.

## Rejected alternatives

- An open session as enough: a stolen cookie would let its holder keep access through a passkey of their own after the session ends.
- A step-up consumed by one change: removing an old passkey right after adding a new one would need a third prompt for no added protection within the same five minutes.
- Keeping the step-up in memory: one more bounded map keyed by session, when the session row already carries per-session state and is deleted with it.
- Replacing a passkey in place (the v2 design's "re-enrol"): an addition followed by a removal does the same without a window where the user has no passkey.
