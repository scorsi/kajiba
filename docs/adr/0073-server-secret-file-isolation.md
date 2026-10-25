# ADR 0073 — Server secret file isolation

Status: Accepted — 2026-10-25

## Context

Round-2 verifier must-fix 1 showed that a server credential outside the server's
state directory, owned by `kajiba-server:kajiba-git` with mode 0640, passed
validation while remaining readable by the git side. Group-writable credentials
also passed. The owner keeps group-readable secret files: trusted secret-manager
groups can deliver credentials without exposing them to the git side.

## Decision

- **Amends 0029 — secret-file validation.** The shared file validator refuses a
  secret that is group-writable or world-accessible (any of the other-user read,
  write or execute bits). Group read access remains allowed. Checks follow
  symlinks and inspect the opened regular file, without logging its contents.
- **Amends 0052 — server credential isolation.** Server startup and
  `kajiba-server config check --files` additionally refuse a group-readable
  runner or mirror credential whose owning gid resolves to the fixed
  `kajiba-git` group. Server-side credential-file reads use the same check.
  A host without that system group can still perform offline checks; lookup
  errors other than a missing group fail closed. Structural `config check`
  continues to touch no secret files.
- The NixOS module asserts the same mode and group restrictions for configured
  server secret paths whose metadata it knows through structured tmpfiles,
  `/etc` entries, sops or agenix declarations. It includes the git user's primary
  and extra groups, explicit group membership and the git unit's supplementary
  groups; known numeric gids and group aliases are included. Files delivered by
  another mechanism are checked at runtime against the fixed `kajiba-git`
  group. Deployments must keep such files unreadable by any additional git-side
  group and must not grant the git side access through ACLs or file ownership.

## Consequences

- A 0640 credential owned by `kajiba-server:kajiba-server` remains valid; one
  owned by `kajiba-server:kajiba-git` is refused even outside server state.
  Modes 0620, 0660 and any world-accessible mode are refused.
- Module evaluation validates declared metadata, not the runtime filesystem.
  Runtime tests cover the actual git user's access and refusal by the server.
  ACL and parent-directory policy remain the deployment's responsibility.
- No dependency is added; group resolution uses the existing libc-backed helper.

## Rejected alternatives

- Refusing every group-readable secret: contrary to the owner's delivery policy.
- Relying only on the git daemon's `InaccessiblePaths` for server state: secrets
  may live elsewhere and SSH helpers run under a different sandbox.
