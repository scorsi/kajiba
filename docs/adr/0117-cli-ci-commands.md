# ADR 0117 — CLI `ci` commands and exit codes

Status: Accepted — 2026-10-31. Amends 0007.

## Context

ADR 0007 names "trigger and follow a CI run" as a first use of `kajiba` and fixes exit code 1 for errors, but not the `ci` commands, what they print where, nor how a script tells a failed run from a run still in progress. The owner decided on 2026-10-30.

## Decision

- **Commands** (`<run>` is a run's public id):
  - `kajiba ci run <owner>/<name> <sha>` triggers a manual run (`RunService.TriggerRun`, ADR 0114) and follows it; `-d`/`--detach` prints the run id and returns at once.
  - `kajiba ci status <run>` prints the run's state without waiting.
  - `kajiba ci watch <run>` re-attaches to a run: prints why it is pending, if it is, then follows it.
  - `kajiba ci logs <run>` prints the run's stored log and returns.
- **Streams.** The run's log goes to stdout, byte for byte. Everything else goes to stderr as `kajiba: …` lines: the run id when `run` starts following, the pending reason (0109), a marker before each attempt's bytes (`attempt <n> on <runner> (<hostname>)`), and the outcome.
- **Failure summary.** When a run has failed, `run`, `watch` and `status` print on stderr `kajiba: run <id> failed[ in check <check>]: <reason>` followed by the last 20 log lines, taken from the run the server sends (the end of the log stream, or `GetRun`), with no extra request (ADR 0112); a cancelled run prints `kajiba: run <id> cancelled`.
- **Exit codes.** `run` and `watch`: 0 when the run succeeded, 1 when it failed or was cancelled. `status`: 0 succeeded, 1 failed or cancelled, 75 (`EX_TEMPFAIL`) queued or running. `logs` and `run --detach`: 0. Any refusal or error stays `kajiba: <message> (<REASON>, request <id>)` with 1 (0007).
- **Following** resumes a dropped log stream from the bytes already received (`WatchRun.offset`), after 1 s, at most 30 times in a row without new bytes; nothing is printed twice.
- **`--json`** prints the run as protojson where the text output describes a run: `status` (text: the state) and `run --detach` (text: the run id). The log itself is never wrapped.

## Consequences

- A script can wait with `kajiba ci watch`, poll with `kajiba ci status` (75 means "ask again"), or pipe `kajiba ci logs`.
- `--detach` and `status --json` give a script the run without parsing text.

## Rejected alternatives

- A distinct exit code for a cancelled run: the owner kept two outcomes for `run` and `watch`, success or not.
- The failure summary on stdout: it would mix with the log a script captures.
