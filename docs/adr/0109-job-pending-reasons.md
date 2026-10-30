# ADR 0109 — Job pending reasons

Status: Accepted — 2026-10-30. Amends 0008, 0013.

## Context

ADR 0008 matches a job only to runners whose capabilities satisfy its requirements, and ADR 0013's `job` table has a `pending_reason` column, but neither says which reasons exist nor how they are chosen. A user looking at a pending run, or an operator reading `kajiba_jobs_pending`, needs to know whether a runner is missing, lacks a backend, lacks a tag or is only busy. The owner decided on 2026-10-30 that the reasons are a fixed set.

## Decision

- **Amends 0008 — matching.** Pending jobs are offered oldest first to the connected runners, using what each announced at admission: the job's backend, its tags as a subset of the runner's, and its `arch` and `os` when the job requires them. A runner takes at most its announced `max_jobs` running attempts. A job requiring no system (a push job) runs on the system of the runner it is matched to (0010). Matching runs when a job is created, when a runner is admitted, when an attempt ends, and at least every second.
- **Amends 0013 — `job.pending_reason`.** A job that no runner can take keeps `pending` with one reason from a fixed kebab-case set, checked in this order:

  | Reason                | Meaning                                                                  |
  | --------------------- | ------------------------------------------------------------------------ |
  | `no-connected-runner` | no runner is connected                                                   |
  | `backend-unavailable` | no connected runner announces the job's backend                          |
  | `tags-unmatched`      | runners have the backend, none has every required tag (or `arch` / `os`) |
  | `runners-busy`        | matching runners exist, all at their `max_jobs`                          |

  The reason is cleared when the job is assigned. A required `arch` or `os` that no runner offers counts as `tags-unmatched`, so the set stays closed.

## Consequences

- The UI and the CLI can show why a run waits without guessing from runner rows.
- Adding a reason is a decision recorded in an ADR, not a free-text change.

## Rejected alternatives

- Free-text reasons: neither a client nor a test could rely on them.
- A separate reason for an unmatched system: no job requires one before worktree runs, and the set was fixed by the owner.
