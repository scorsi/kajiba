# ADR 0062 — Push input and quarantine limits

Status: Accepted — 2026-10-25

## Context

J3 security review item 2 identifies unbounded storage consumption before push
signature admission. An authorised writer reaches `receive-pack` before the
pre-receive hook checks signatures (0003, 0052, 0053). A wire-pack limit alone
does not bound loose-object expansion or the base objects appended when Git
repairs a thin pack.

## Decision

- **Amends 0003 — receive-pack transport.** Every receive-pack command overrides
  repository configuration with `receive.maxInputSize=67108864`: at most 64 MiB
  of incoming pack data per push. `receive.unpackLimit=0` keeps even small pushes
  packed, avoiding loose-object expansion. These are fixed product limits, not
  deployment options; upload-pack and admin import are unaffected.
- The receive-pack child and its descendants have both soft and hard
  `RLIMIT_FSIZE` capped at 128 MiB (134,217,728 bytes), preserving any stricter
  inherited limit. Failure to install the limit prevents spawning. This bounds
  individual files while Git receives, repairs and indexes the pack, before
  any hook can run. It also applies to ref files written by that child.
  `receive.autogc=false` keeps repository-wide repacking out of this restricted
  push process; maintenance remains separate (0052).
- **Amends 0052 and 0053 — git-side pre-receive checks.** Before walking objects,
  checking ancestry or verifying signatures, the git side totals regular-file
  lengths in the hook's object and quarantine directories, counting identical
  paths once. The aggregate must be at most 128 MiB, including pack indexes and
  appended thin-pack bases. Missing or unreadable paths, symlinks, special files
  and an exceeded budget fail closed. An absent quarantine remains valid for
  deletion-only pushes. Existing repository objects used as alternates are not
  counted. Rejection reaches the hook before server admission or a pending row.

## Consequences

- Large pushes must be split into smaller pushes; a pack below the wire limit
  can still be refused after thin-pack repair or indexing. Refusal occurs before
  ref updates, and Git's normal receive-pack failure cleanup removes quarantine.
- The 128 MiB aggregate is an admission limit, not a filesystem quota: during
  ingestion multiple individually capped files can coexist, and filesystem
  metadata and allocation overhead are additional. Keeping objects packed and
  bounding each file prevents unbounded per-push disk expansion before the
  aggregate check.
- This decision does not cap cumulative accepted repository data, simultaneous
  pushes to different repositories, imports or the append-only event journal.
  Filesystem quotas, minimum-free-space admission and reserved database space
  remain outside this fix; no audit events or accepted objects are deleted.

## Rejected alternatives

- Checking size only in pre-receive: the input can already have exhausted disk.
- Only limiting incoming pack bytes: loose-object expansion and appended
  thin-pack bases can exceed that budget before signature checks.
- Deployment-specific quota management in the Git transport: a separate storage
  policy and operational contract, not required for these fixed per-push limits.
