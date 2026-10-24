# ADR 0013 — Data model and ref namespaces

Status: Accepted — 2026-10-09 (revised 2026-10-24: relational v0 schema accepted, declared identity)

## Context

Later phases (issues, external identities, external merge requests, private branches) must fit in without a redesign. An external contribution never triggers CI automatically.

## Decision

Entities present from v0 (some empty); columns, keys and constraints in **[docs/schema.md](../schema.md)**, the reference for migration `0001`:

- Identity: `user` (`kind`: `local` | `external`, `is_admin`, `source`: `config` | `runtime`), `ssh_key` (usage: auth / signing), `api_token` (hashed, scopes, expiration, `source`), `webauthn_credential`, `session`, `enrollment_token` (0014).
- `repo` (owner, name, visibility — `public` only in v0, CI enabled or not), `repo_redirect` (0021), `repo_permission` (user, repo, role).
- `mirror` (repo, remote URL, credential name, queue state, status, last error, attempts).
- `runner` (name, capabilities, version, last contact), `run`, `job`, `job_attempt`, `step`, `log_chunk`, `artifact` (0008).
- `event`: **append-only** journal (push by principal, run, mirror, auth); source of triggers and audit. `handler_checkpoint` (0017), `pending_push` (0003).
- Reserved for later, schema sketched but not created: `oauth_identity`, `issue`, `merge_request` (source repository, branch, fetched sha).
- Every exposed entity has an integer key (internal) and a `public_id` (ULID, unique, 0020).
- Items declared in the configuration (`source = config`) are reconciled at startup and read-only through the API (0014).

Ref namespaces:

- `refs/heads/*`, `refs/tags/*`: **trusted** refs, written only by `local` users with the required role; the only mirrored refs (0011) and the only refs triggering CI (0010).
- `refs/contrib/<user>/<branch>`: branches **fetched by the forge** for external merge requests; never written by a push, never mirrored, never automatic CI; manual trigger only.
- Private branches (later): dedicated prefix or DB marker; excluded from the mirror and from public visibility.
- Any other prefix is refused in `pre-receive`.

## Consequences

- A single, readable trigger rule: "`push` event on a trusted ref by a trusted principal".
- `pre-receive` is the single control point for refs (0014).

## Rejected alternatives

- Contribution refs in `refs/heads/` with protection: fragile, mixes trust and third parties.
- Internal forks for contributors: the forge would serve as storage for third parties, explicitly refused.
