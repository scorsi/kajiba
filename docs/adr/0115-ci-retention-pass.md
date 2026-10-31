# ADR 0115 — CI retention pass

Status: Accepted — 2026-10-31. Amends 0010, 0019, 0022.

## Context

ADR 0010 gives CI logs a short retention "cleaned up by a timer", ADR 0022 lists a daily `kajiba-retention.timer` declared by the module, and ADR 0019 names `ci.log_retention` without saying when the cleanup runs nor what it keeps. The owner decided on 2026-10-30, taking azukari's retention as a model: an in-process daily pass at a configured local time, also run on the operator's demand, with durations short enough for a test.

## Decision

- **Amends 0010 and 0022 — in-process, no timer.** `kajiba-server` runs the retention pass itself once a day at the local time `ci.run_at`, and on demand through `kajiba-server admin retention` over the local socket (`kajiba.admin.v1` `Retention`, 0093). The module declares no `kajiba-retention.timer`. There is no dry-run mode.
- **What goes.** The log file (`ci/logs/<attempt>.log`) and the `log_chunk` rows of every attempt finished more than `ci.log_retention` ago; the file first, then its rows, so an interrupted pass leaves rows the next pass deletes. Runs, jobs, attempts and steps are kept, with their failure reason; a run whose logs are gone shows an empty log and a summary without log lines. A running attempt is never touched.
- **Amends 0019 — one key, positive durations.**

  | Field       | Default | Meaning                                                  |
  | ----------- | ------- | -------------------------------------------------------- |
  | `ci.run_at` | `03:00` | local time `HH:MM` (24-hour) of the daily retention pass |

  `ci.log_retention` and `ci.artifact_retention` take `d`, `h`, `m` or `s` and must be positive; there is no floor, so a test can run retention within minutes.

- The pass logs `retention-completed{logs}` (the number of attempts whose logs were deleted), or `retention-failed{err}`; the admin command prints the same count.

## Consequences

- Retention needs no unit beside the server and follows its configuration on restart.
- A minute-long retention is accepted: a deployment that sets one loses its logs within minutes.

## Rejected alternatives

- A systemd timer: a second unit and a second entry point for one SQL query and a few file deletions, and nothing on a host without systemd.
- A dry-run mode: the pass only deletes logs past a retention the operator chose.
