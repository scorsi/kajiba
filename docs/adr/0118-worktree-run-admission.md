# ADR 0118 — Worktree run admission: check order and where it applies

Status: Accepted — 2026-10-31. Amends 0097.

## Context

ADR 0097 lists the refusals of a worktree run in table order: scope, readable, `write` role, archived, CI disabled, names, no matching runner, active-run limit. An archived repository caps every role at `read` (0026), so in that order a `write` user of an archived repository is told `ROLE_MISSING`, never `INVALID_STATE`: the archived row is unreachable for exactly the users it concerns. ADR 0114 met the same problem for `TriggerRun` and checks the archive before the role. ADR 0097 also gives `GetWorktreeBase` the repository alone, so a request bound to be refused (CI off, no runner for the system, too many active runs) is only refused after the client has built and started uploading a bundle.

## Decision

- **Order** (amends 0097's table order, as 0114 for `TriggerRun`): credential with `ci:trigger` (`SCOPE_MISSING`); repository readable (`REPO_NOT_FOUND`); not archived (`INVALID_STATE`); `write` role (`ROLE_MISSING`); CI enabled (`CI_DISABLED`); check, system and feature names (`INVALID_FIELD`); a declared runner able to match (`NO_MATCHING_RUNNER`); at most 4 active worktree runs of the caller (`WORKTREE_RUNS_LIMIT`). Codes are those of 0097.
- **One check, both methods.** `GetWorktreeBase` takes the same description of the run as `CreateWorktreeRun`'s first message (repository, system, checks, features) and makes the same checks before answering, so the client is refused before it builds a bundle. `CreateWorktreeRun` repeats them on its first message, the active-run limit again under the write lock that creates the run.
- **Names.** A check name is `[A-Za-z0-9_-]+`; a system is `<arch>-<os>`, each half `[a-z0-9_]+`; a feature is a runner tag (0107: lowercase ASCII letters, digits, `.`, `_`, `-`, 1 to 63 characters), since it is matched as one. A well-formed system no declared runner has is `NO_MATCHING_RUNNER`, not `INVALID_FIELD`.
- **Matching.** A declared runner can match when its `arch` and `os` are the system's halves, its backends include `nix` and its tags include every feature; whether it is connected does not matter (0097).

## Consequences

- A `write` user of an archived repository is told it is archived, for manual and worktree runs alike.
- `GetWorktreeBase` needs `ci:trigger` and the `write` role: the ref tips it returns are readable through browsing anyway, and only a caller allowed to create the run needs them.

## Rejected alternatives

- 0097's table order: `INVALID_STATE` would never reach a `write` user.
- `GetWorktreeBase` with the repository alone: refusals known before the upload would only come after it.
