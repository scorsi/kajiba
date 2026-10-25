# ADR 0069 — Orphan quarantine by immediate directory name

Status: Accepted — 2026-10-25

## Context

ADR 0032 requires every unregistered directory under `repos/` to be quarantined
at startup. ADR 0053 preserves that sweep but restricts `QuarantineRepository`
to a repository public id. A directory such as `orphan.git`, a name without the
`.git` suffix, or a non-UTF-8 filename cannot be expressed by that request and
currently aborts startup. J3 review quality item 7 identifies this mismatch;
note 16 identifies the missing server-side `repo-orphaned` event.

## Decision

- **Amends 0053 — `QuarantineRepository` and the public-id-only invariant.**
  This RPC takes `directory_name`, the bytes of one immediate entry under
  the git side's `repos/`, instead of a public id. The old protobuf field number
  and name are reserved. Bytes preserve Unix filenames without requiring UTF-8.
  Other repository RPCs continue to use public ids.
- The daemon rejects empty names, `.` and `..`, NUL bytes and `/` anywhere in
  the name. It constructs the path beneath its configured `repos/`, requires a
  real directory, and refuses symlinks and non-directories. No arbitrary path,
  deletion or Git command is exposed.
- ADR 0067's eligibility rules apply unchanged to the named directory: it must
  be a valid repository with no refs, no stored objects and no active session
  (a session matches when the name is `<repo_public_id>.git`). Any other
  directory is left in place, yielding `FAILED_PRECONDITION`/`INVALID_STATE`.
  An entry named `<id>.git` shares that id's mutation lock.
- The server still compares directory names with every registered repository,
  including archived ones, before requesting quarantine. The daemon remains
  the only writer: it moves the directory into its `tmp/orphans/`, preserves
  filename bytes when appending collision suffixes, and syncs both parents.
- ADR 0032's `repo-orphaned{path,destination}` warning remains required on the
  server after a successful RPC, using the daemon's returned destination.
  ADR 0053's daemon-side `repo-quarantined` warning remains unchanged. The
  response and log paths are display strings; non-UTF-8 bytes are replaced for
  display only, never when choosing the filesystem source or destination.

## Consequences

- Startup quarantines unregistered directories regardless of their naming,
  preserving their contents and earlier quarantines as ADR 0032 requires.
- Both binaries and generated clients change together under ADR 0053's existing
  exact-version deployment rule. A request using only the retired field is
  rejected as an empty directory name.
- Tests cover non-public-id and non-UTF-8 names, containment validation,
  symlinks, collisions, durability, registered-directory preservation and the
  restored server warning. No dependency is added.

## Rejected alternatives

- Refusing startup for non-public-id names: contradicts ADR 0032's orphan sweep.
- Moving directories in the server: violates ADR 0052's single writer boundary.
- Accepting a path or a UTF-8-only name: broadens filesystem authority or still
  leaves some Unix directory names impossible to quarantine.
