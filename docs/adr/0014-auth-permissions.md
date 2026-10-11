# ADR 0014 — Authentication and permissions

Status: Accepted (merge from the UI: Open) — 2026-10-09

## Context

A single user with FIDO2 keys, possible friends later, GitHub identities for outside people after that. Signed commits mandatory.

## Decision

- **Web: WebAuthn / passkeys only** (webauthn-rs), no passwords. Session through an HttpOnly cookie. The first user is created by a bootstrap command (`kajiba admin bootstrap`) that issues a single-use enrollment link.
- **CLI and scripts: personal access tokens** (PAT) hashed in the DB, scopes (`repo:read`, `repo:write`, `ci:trigger`, `admin`…), optional expiration. Device flow later.
- **SSH**: keys registered per user (0004).
- **Permissions: per-repo RBAC** — roles `read`, `write`, `admin` — plus global `is_admin`. `user.kind = external` never has a write role; read only on what the forge chooses to expose. Prepared for per-ref-pattern rules (gitolite style) without changing the model.
- **Single control point**: `pre-receive` checks, for each updated ref, the allowed prefix (0013), the role, force-push, and the **signature of every new commit** against the pusher's signing keys (`allowed_signers` generated from the DB). Every refusal is explicit.
- **Open**: merge from the UI (fast-forward only, or merge commit signed by a key owned by the forge). To be decided when merge requests exist.

## Consequences

- No password secret to store; losing both FIDO2 keys = re-bootstrap through the admin command on the host.
- Commits produced by the forge (merge, later) will have to be signed or forbidden.

## Rejected alternatives

- Password + TOTP: useless with passkeys.
- Per-ref ACL from v0: premature; the model accommodates it later.
