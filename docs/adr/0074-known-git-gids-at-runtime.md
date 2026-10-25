# ADR 0074 — Known git GIDs at runtime

Status: Accepted — 2026-10-25

## Context

Round-3 verifier must-fix 1 found that a numeric supplementary group on the git
unit bypassed the module's secret isolation check when the credential used the
group's name. Runtime validation only compared the fixed `kajiba-git` GID.

## Decision

- **Amends 0073 — known numeric groups.** Numeric entries in the git unit's
  supplementary groups join the module's effective GID set directly, alongside
  GIDs resolved from declared group names. Named secret groups and aliases are
  compared against this complete set.
- **Amends 0019 and 0073 — runtime validation.** The module writes this known GID
  set to `git.group_ids` in the server TOML, through the read-only derived option
  `services.kajiba-server.git.groupIds`. Its code default is an empty list; the
  module omits an empty set. Server startup and `config check --files` refuse a
  group-readable runner or mirror credential whose opened file GID is in this
  set, in addition to the existing fixed `kajiba-git` group check. Structural
  checks continue to touch no credential files.

## Consequences

- A numeric supplementary group cannot bypass either the declared-metadata
  check or the runtime check of configured runner and mirror credentials.
- The fixed-group fallback and missing-system-group offline behavior remain.
  Additional groups without known GIDs, ACLs, ownership and parent-directory
  isolation remain deployment responsibilities under 0073.
- No dependency is added. Module and Rust regressions cover the numeric GID
  handoff and rejection while preserving owner-only and unrelated-group access.

## Rejected alternatives

- Fixing only declared metadata: the runtime filesystem may differ from its
  declaration.
- Discovering supplementary groups from the git account alone: systemd can add
  numeric groups to the unit without adding account memberships.
