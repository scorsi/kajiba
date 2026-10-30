# ADR 0108 — CI journal events

Status: Accepted — 2026-10-30. Amends 0017, 0022.

## Context

ADR 0017 journals every event in the transaction of the change it reports and names `run-finished` as an example kind; ADR 0022 lists the CI log lines (`run-state`, `runner-connected`, `runner-refused`). Neither lists which CI facts are journaled nor their payloads. The owner decided on 2026-10-30 that every CI fact is appended to the journal, so a later handler (notifications, the UI's activity) can replay them.

## Decision

- **Amends 0017 — CI event kinds.** Each is appended in the transaction of the change it reports, `version` 1. Identifiers in payloads are public ids; a runner is named by its declared name.

  | Kind                  | Principal                                                                 | `repo_id` | Payload                                                                              |
  | --------------------- | ------------------------------------------------------------------------- | --------- | ------------------------------------------------------------------------------------ |
  | `run-created`         | the user who triggered it (the pusher, or the caller of a manual run)     | the run's | `{run, trigger, ref, sha}` (`ref` null when the run has none)                        |
  | `run-started`         | the runner whose acceptance started the run's first attempt               | the run's | `{run, attempt, runner}`                                                             |
  | `run-finished`        | the runner reporting the result, or `system` when the server ends the run | the run's | `{run, state, check, reason}` (`check` and `reason` null when unknown or on success) |
  | `attempt-lost`        | `system`                                                                  | the run's | `{run, attempt, runner}`                                                             |
  | `attempt-replayed`    | `system`                                                                  | the run's | `{run, job, attempt}`, `attempt` being the lost one the replay follows               |
  | `runner-connected`    | the runner                                                                | none      | `{runner, version, hostname}`                                                        |
  | `runner-disconnected` | the runner                                                                | none      | `{runner}`                                                                           |
  | `runner-refused`      | the runner                                                                | none      | `{runner, reason}`                                                                   |

- **Refusals of unauthenticated callers are not journaled.** `runner-refused` is journaled only for a caller that proved the declared secret: reasons `version` and `capabilities`. An unknown name (`unknown`), a wrong secret (`secret`) and a disabled runner (`disabled`: removed from the configuration, so no secret is declared to check) are only logged (`runner-refused{runner,reason}`, warn) and counted, so a caller without the secret cannot grow the journal.
- **Amends 0022 — log lines.** `runner-connected` gains `hostname`; `runner-disconnected{runner}` is added. The `runner-refused` reasons are `unknown`, `disabled`, `secret`, `version` and `capabilities`. The metric `kajiba_runner_refusals_total{reason}` counts every refusal, journaled or not.

## Consequences

- The journal holds a complete CI history apart from refusals without the secret; a handler added later replays it from its checkpoint (0017).
- A run created by the `ci` handler and its `run-created` event share the push event's correlation id.

## Rejected alternatives

- Journaling only `run-finished`, as 0017's example suggests: a later consumer could not tell when runs were created or replayed without reading mutable tables.
- Journaling every refusal: any caller reaching the public port could append rows without a credential.
