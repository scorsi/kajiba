# ADR 0133 — Per-repository CI mode: off, manual, push

Status: Accepted — 2026-10-31. Amends 0010, 0013, 0026, 0097, 0114, 0118.

## Context

A declared repository has one boolean, `ci_enabled` (0026), that gates both kinds of runs: a push on a trusted ref creates a run only when it is true (0010), and `TriggerRun` (0114) and worktree runs (0097, 0118) are refused with `CI_DISABLED` when it is false. A repository whose checks should run on demand only (`kajiba ci run`, `kajiba ci run --worktree`), without spending runner time on every push, cannot be declared. The owner decided on 2026-10-31.

## Decision

- **Amends 0026 — the field.** `repos.<key>.ci_enabled` is replaced by `repos.<key>.ci`, one of:

  | Mode     | Push on a trusted ref | `TriggerRun`, `ci run --worktree` |
  | -------- | --------------------- | --------------------------------- |
  | `off`    | no run                | refused, `CI_DISABLED`            |
  | `manual` | no run                | allowed                           |
  | `push`   | one run (0010)        | allowed                           |

  The default is `off`, as `ci_enabled = false` was; `ci_enabled = true` is `push`. The server configuration refuses `ci_enabled` as an unknown field. The NixOS option `services.kajiba-server.repos.<key>.ci` is `null` (the default, `off`) or one of the three modes; the former `ciEnabled` option fails evaluation with an assertion naming the replacement, so a deployment migrates in one visible step rather than through a silent alias.

- **Amends 0010 and 0013.** The `ci` handler creates a run for a push only in mode `push`; the repository row stores the mode (`repo.ci_mode`), and a migration maps the stored boolean (`1` → `push`, `0` → `off`).
- **Amends 0114, 0118 and 0097.** The admission check named "CI enabled" passes in modes `manual` and `push`; in mode `off` it refuses as before, with the same code, reason and place in the order (`FAILED_PRECONDITION`/`CI_DISABLED`).
- **API.** `Repo.ci_enabled` (field 7) is replaced by `Repo.ci_mode`, an enum `CiMode` (`CI_MODE_OFF`, `CI_MODE_MANUAL`, `CI_MODE_PUSH`); field 7 is reserved. The CLI and the UI show the mode.

## Consequences

- A deployment's configuration changes once: `ciEnabled = true` becomes `ci = "push"`, `ciEnabled = false` is removed (or becomes `ci = "off"`).
- A repository can rely on manual and worktree runs only, with no run per push.
- Refusals are unchanged for every repository that had CI disabled.

## Rejected alternatives

- Two booleans (`ci_enabled` and `ci_on_push`): one of their four combinations (push runs without manual ones) means nothing.
- Keeping `ciEnabled` as a deprecated alias of `ci`: two options for one setting, kept forever, while the migration is one line.
