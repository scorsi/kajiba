# ADR 0025 — Security review decisions for J1

Status: Accepted — 2026-10-24

## Context

A defensive security review of J1 found gaps that the accepted ADRs left open. Fixing them needs decisions on points of ADRs 0014, 0019 and 0022. As README says, an accepted ADR is not edited: each item below names the ADR point it supersedes or completes. Everything else in those ADRs stands.

## Decision

- **Amends 0014: removing a user.** A user removed from the configuration is disabled, as before: its sessions are deleted and its runtime PATs revoked. In the same transaction, its pending enrollment link (`enrollment_token`) and its passkeys (`webauthn_credential`) are now deleted too. Declaring the user again re-enables the account with no passkey and no link, so the user needs a fresh enrollment (`admin enroll`). `admin enroll` still refuses a disabled user (`FAILED_PRECONDITION`, `INVALID_STATE`, ADR 0024).
- **Amends 0019: TTL bounds.** `config check` accepts `auth.enrollment_ttl` from 1 minute to 24 hours and `auth.session_ttl` from 1 minute to 90 days, and refuses any other value, naming the field. Expiries are computed with a checked conversion to milliseconds and a checked addition. A value that cannot be represented is an error and nothing is written. It never wraps around.
- **Amends 0014 and 0019: entropy of declared PATs.** A declared token must carry at least 128 bits of randomness. Its SHA-256 sits in the configuration, which is world-readable on the host, so a low-entropy token could be brute-forced offline. Validation cannot check this from a hash, so it is documented on `users.<name>.tokens.<token>.sha256` (module option and configuration type). The documentation includes a one-line recipe that prints a token in ADR 0014's format (`kjb_` + 32 random bytes in base64url, 256 bits) and its hash:
  `t="kjb_$(head -c 32 /dev/urandom | basenc --base64url | tr -d '=')"; echo "$t"; printf %s "$t" | sha256sum`.
  No subcommand generates tokens.
- **Amends 0014: a declared key wins over a runtime key.** At startup, a runtime SSH key with the same fingerprint and usage as a declared key is deleted (the `ssh_key` table has no revocation column), and the declared key is inserted. Startup does not abort. Each deletion is logged. This applies whichever user owned the runtime key. Runtime PAT collisions with declared tokens still abort reconciliation, because this decision covers keys only.
- **Amends 0022: events added to the list.**
  - `accept-failed{err}` (warn): accepting a connection on the public, metrics or local listener failed with an error other than a peer giving up (for example `EMFILE`). The listener backs off for one second before accepting again.
  - `runtime-key-revoked{user,usage,fingerprint}` (warn): reconciliation deleted `user`'s runtime SSH key because a declared key has the same SHA256 `fingerprint` and `usage` (`auth` or `signing`).

## Consequences

- Removing a user from the configuration ends all of its access at the next start. Re-adding the user is a new enrollment, never a silent return of old passkeys or of a link issued before the removal.
- A configuration with a TTL outside its range fails at build time (`config check` on the generated file), not at runtime.
- Operators learn the entropy requirement from the option documentation. Nothing enforces it.
- A key an operator declares always takes effect, even if a user added the same key at runtime. The runtime owner loses that key, and the log says so.

## Rejected alternatives

- Keeping passkeys when a user is disabled, so that re-declaring brings them back: an operator who removes a user expects the removal to be complete.
- Aborting startup when a runtime key collides with a declared key: one user's runtime action could then stop the whole forge, and `config check` cannot see the database.
- A `kajiba-server` subcommand that generates tokens: a shell one-liner does the same job, and adding a command would grow the surface.
- Unbounded TTLs with saturating arithmetic: a typo such as `90000d` would silently mean "forever".
