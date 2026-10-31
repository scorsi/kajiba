# ADR 0114 — Manual run trigger: scope, role and commit

Status: Accepted — 2026-10-31. Amends 0010, 0014.

## Context

ADR 0010 allows a manual "run on this sha" for users with the required scope, and 0013 makes it the only CI path for an external contribution (`refs/contrib/*`). ADR 0014 lists the `ci:trigger` scope without saying which role goes with it, nor which commits may be run. The owner decided on 2026-10-30.

## Decision

- **Amends 0014 — scope and role.** `RunService.TriggerRun` requires a credential holding `ci:trigger` (else `PERMISSION_DENIED`/`SCOPE_MISSING`) **and** the `write` role on the repository. Running a commit spends runner time and can make the runner fetch anything during evaluation (0009): reading the repository is not enough.
- **Amends 0010 — which commit.** Any commit reachable from any ref of the repository, `refs/contrib/*` included, can be run; a commit that is unknown or reachable from no ref is `NOT_FOUND`/`REVISION_NOT_FOUND`, so a manual run never builds an object only present in the store (a rejected push's leftovers, an object received but no longer referenced).
- **Checks, in order**: repository readable (else `NOT_FOUND`/`REPO_NOT_FOUND`); not archived (else `FAILED_PRECONDITION`/`INVALID_STATE`, checked before the role because an archived repository caps every role at `read`, 0026); `write` role (else `PERMISSION_DENIED`/`ROLE_MISSING`); CI enabled (else `FAILED_PRECONDITION`/`CI_DISABLED`); the commit. Nothing is written before the last check passes.
- The run is `manual`, without a ref, with the caller as `triggered_by`; it is created with its job and `run-created` in one transaction (0108), as a push run.

## Consequences

- An external contribution is run by a `write` user's explicit decision, as 0013 intends.
- A `ci:trigger` token alone cannot read logs: `GetRun`, `ListRuns` and log streaming need `repo:read`.

## Rejected alternatives

- `ci:trigger` with the `read` role: any reader holding such a token could make the runners execute arbitrary commits of a public repository.
- Accepting any commit present in the store: objects outside every ref are not part of the repository's history and may be garbage.
