# ADR 0131 — `kajiba ci list`

Status: Accepted — 2026-10-31. Amends 0117.

## Context

ADR 0117's `ci` commands all take a run id, which a user only sees when triggering a run from the CLI; a run started by a push, or by someone else, has no command-line way to find it. `RunService.ListRuns` already lists a repository's runs, newest first, but cannot select by state. The owner decided on 2026-10-31.

## Decision

- **Amends 0117 — `kajiba ci list`** prints a repository's recent runs, newest first, one per line: public id, state, trigger, the commit's first 12 hex digits, the ref (`-` for a manual or worktree run) and the creation time (RFC 3339, UTC), separated by two spaces. It exits 0, empty output included.
  - The repository is the one this clone's remotes address on the forge, inferred as by `ci run --worktree` (0097); `--repo <owner>/<name>` (or a public id) names another.
  - `--state <state>` keeps runs in that state (`queued`, `running`, `succeeded`, `failed`, `cancelled`); repeatable or comma-separated, any of them matches.
  - `--limit N` (default 20, at least 1) bounds the runs printed; the CLI follows `ListRuns` pages until it has them or none is left.
  - `--json` prints the runs as a protojson array of `kajiba.v1.Run`, as `repo list` does.
- **`ListRunsRequest.states`** (repeated `RunState`, field 4): empty lists every run; otherwise only runs in one of the given states. An unspecified or unknown value is `INVALID_FIELD`. The filter is applied before pagination; a cursor stays bound to its repository only (0020).

## Consequences

- `kajiba ci list --state running` finds the run a push started, for `ci watch`.
- A server older than the field would ignore it and list every state; before 1.0, ADR 0023 refuses such a version mix anyway.

## Rejected alternatives

- Filtering by state in the CLI: a rare state could page through the repository's whole history.
- A server-side default limit or a `limit` field: `page_size` and the cursor already bound each answer.
