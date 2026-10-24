# ADR 0014 — Identity, authentication and permissions

Status: Accepted — 2026-10-09, revised 2026-10-24 (declarative identity and enrollment: Accepted; lifecycle values and signature algorithm: accepted; merge from the UI: Open)

## Context

One owner at first, possibly a small team, GitHub identities for outside people later. Signed commits mandatory. The forge is deployed only through Nix: everything that can be declared is declared, so the deployment repository is the source of truth for who exists and which keys they hold.

## Decision

### Declarative identity (Accepted)

- **No separate administrator account**: the owner's user has `is_admin`.
- **Declared in the Nix-generated configuration** (0019): users, `is_admin`, SSH public keys (auth), signing keys, and PATs as **hashes** (`sha256`, scopes, optional expiry; the token itself is a secret file held by its user, never in the configuration). The server **reconciles** them into the DB at startup (`source = config`): added, updated, and removed — a removed user is disabled (its sessions and runtime PATs revoked, its repositories kept), a removed key or token is deleted. The UI and the API **refuse to modify declared items** (`FAILED_PRECONDITION`, reason `DECLARED_IN_CONFIG`, 0020).
- **Passkeys are the one runtime state**: WebAuthn credentials are created by a browser ceremony. A user gets an **enrollment link** from a host-local command, `kajiba-server admin enroll <user>`, which prints a single-use URL (expires after `auth.enrollment_ttl`, default 15 min; token stored hashed, consumed atomically; a new link replaces the previous one). Further passkeys are added from an authenticated session.
- **Recovery = the same command**: `admin enroll` revokes the user's passkeys, sessions and runtime-created PATs before issuing the link; declared keys and tokens stay. This command requires root on the forge host (or the `kajiba` service user, which owns the database anyway); root already holds every secret of the forge: recovery is an operational procedure, not a security boundary.

### Authentication

- **Web: WebAuthn / passkeys only** (webauthn-rs), no passwords. Session through an HttpOnly cookie.
- **CLI and scripts: personal access tokens** (PAT), declared (above) or created at runtime from an authenticated session; hashed in the DB, scopes (`repo:read`, `repo:write`, `ci:trigger`, `admin`…), optional expiration. Device flow later.
- **SSH**: declared keys (0004).
- **Lifecycle values**: relying party ID = host of `http.public_url`, origin = `http.public_url` (0019); ceremony challenge 5 min (in memory); session `auth.session_ttl` (default 7 days, absolute), logout deletes the session row; PAT format `kjb_` + 32 random bytes in base64url, stored as SHA-256, shown once. Each RPC declares its required scope as a proto method option; the interceptor enforces it and a unit test fails on any method without one. A browser session holds every scope of its user.

### Permissions

- **Per-repo RBAC** — roles `read`, `write`, `admin` — plus global `is_admin`. A repository's owner is `admin` on it. `user.kind = external` never has a write role; read only on what the forge chooses to expose. Prepared for per-ref-pattern rules (gitolite style) without changing the model.
- **Single control point**: `pre-receive` checks, for each updated ref, the allowed prefix (0013), the role (non-fast-forward updates and deletions require `admin`; deleting the default branch is refused), and signatures.
- **Signature algorithm**: the commits to check are `git rev-list <new tips> --not <all refs before the push>`, computed once for the whole push inside git's quarantine environment (the hook inherits it), so a first push to an empty repository checks its whole history and a commit shared by two pushed refs is checked once. Each must carry a valid SSH signature (`gpg.format=ssh`) by a **signing key of the pusher**, verified with an `allowed_signers` file generated from the DB. Tags: only **annotated, signed** tags are accepted (same key rule); lightweight tags are refused. Deletions carry nothing to verify. Every refusal names the ref, the commit or tag and the reason.
- **Open**: merge from the UI (fast-forward only, or merge commit signed by a key owned by the forge). To be decided when merge requests exist.

## Consequences

- No password secret to store; losing every passkey = `admin enroll` on the host.
- Adding a user or a key is a deployment change, reviewed like any other.
- Commits produced by the forge (merge, later) will have to be signed or forbidden.

## Rejected alternatives

- A bootstrap command creating the first admin in the DB: a second source of truth for users next to the configuration.
- Passkeys declared in the configuration: a WebAuthn credential only exists after a ceremony with the authenticator.
- Password + TOTP: useless with passkeys.
- Per-ref ACL from v0: premature; the model accommodates it later.
