# ADR 0028 — Identity lifecycle and enrollment links

Status: Accepted — 2026-10-24

## Context

J1 implemented enrollment links and the reconciliation of declared users and keys (0014), and its security review found gaps in what happens when a user is removed or a declared credential meets a runtime one. ADR 0014 left these points implicit. As README says, an accepted ADR is not edited: each item below names the ADR point it amends; everything else in that ADR stands. Each item describes what the J1 code does and is covered by its tests.

## Decision

- **Amends 0014 — enrollment links.**
  - Format: `<public_url>/auth/enroll#<token>`, where the token is 64 lowercase hexadecimal characters (32 random bytes). The token is in the URL fragment, which browsers never send to servers or proxies, so it stays out of request and proxy logs. The database stores its SHA-256 only.
  - `admin enroll` for a disabled user fails with `FAILED_PRECONDITION` (reason `INVALID_STATE`); for an unknown user, with `NOT_FOUND` (reason `USER_NOT_FOUND`).
  - Issuing a link writes no event to the journal (0017); it is logged as `enrollment-issued{user}` only, never with the link or the token.
  - That issuing a new link invalidates the previous one is verified in tests through the database (`enrollment_token`), until an admin RPC lists outstanding links.
- **Amends 0014 — removing a user.** A user removed from the configuration is disabled, as before: its sessions are deleted and its runtime PATs revoked. In the same transaction, its pending enrollment link (`enrollment_token`) and its passkeys (`webauthn_credential`) are deleted too. Declaring the user again re-enables the account with no passkey and no link, so the user needs a fresh enrollment (`admin enroll`). `admin enroll` still refuses a disabled user (`FAILED_PRECONDITION`, `INVALID_STATE`, above).
- **Amends 0014 — a declared key wins over a runtime key.** At startup, a runtime SSH key with the same fingerprint and usage as a declared key is deleted (the `ssh_key` table has no revocation column), and the declared key is inserted. Startup does not abort. This applies whichever user owned the runtime key. Runtime PAT collisions with declared tokens still abort reconciliation, because this decision covers keys only.
- **Amends 0022 — event added to the list.** `runtime-key-revoked{user,usage,fingerprint}` (warn): reconciliation deleted `user`'s runtime SSH key because a declared key has the same SHA256 `fingerprint` and `usage` (`auth` or `signing`).
- **Amends 0014 and 0019 — entropy of declared PATs.** A declared token must carry at least 128 bits of randomness. Its SHA-256 sits in the configuration, which is world-readable on the host, so a low-entropy token could be brute-forced offline. Validation cannot check this from a hash, so it is documented on `users.<name>.tokens.<token>.sha256` (module option and configuration type). The documentation includes a one-line recipe that prints a token in ADR 0014's format (`kjb_` + 32 random bytes in base64url, 256 bits) and its hash:
  `t="kjb_$(head -c 32 /dev/urandom | basenc --base64url | tr -d '=')"; echo "$t"; printf %s "$t" | sha256sum`.
  No subcommand generates tokens.

## Consequences

- Removing a user from the configuration ends all of its access at the next start. Re-adding the user is a new enrollment, never a silent return of old passkeys or of a link issued before the removal.
- A key an operator declares always takes effect, even if a user added the same key at runtime. The runtime owner loses that key, and the log says so.
- Operators learn the entropy requirement from the option documentation. Nothing enforces it.

## Rejected alternatives

- Putting the enrollment token in the query string or the path: it would appear in proxy and request logs.
- Keeping passkeys when a user is disabled, so that re-declaring brings them back: an operator who removes a user expects the removal to be complete.
- Aborting startup when a runtime key collides with a declared key: one user's runtime action could then stop the whole forge, and `config check` cannot see the database.
- A `kajiba-server` subcommand that generates tokens: a shell one-liner does the same job, and adding a command would grow the surface.
