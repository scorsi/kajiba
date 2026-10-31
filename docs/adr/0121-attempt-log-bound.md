# ADR 0121 — Stored log bound per attempt

Status: Accepted — 2026-10-31. Amends 0008, 0022 and 0115.

## Context

A runner's log chunks are bounded in size (0105) and the runner's unacknowledged backlog is bounded by backpressure, but nothing bounded the total: a noisy check could fill the forge's `ci/logs` and the `log_chunk` index for as long as it runs, and retention (0115) only removes finished attempts' logs. The owner decided on 2026-10-31.

## Decision

- **Bound.** The forge stores at most **64 MiB** of log bytes per attempt, a product constant, not configuration.
- **Past it.** The chunk that crosses the bound is stored up to the bound, followed by one marker line, `[kajiba: log truncated: an attempt stores at most 64 MiB of log]`, on its own line. Later chunks of the attempt are acknowledged in sequence as if stored, so the runner's backlog drains, and are discarded without a new index row. The job continues: an attempt is never failed or killed for its log volume, and its result is recorded as usual.
- **Metric** (amends 0022): `kajiba_logs_truncated_total` (counter, no label) counts attempts whose log was truncated, once each.

## Consequences

- One attempt costs at most 64 MiB plus the marker on the forge's disk, and its index stops growing once truncated; readers (`WatchRun`, `ci logs`, failure summaries) see the marker as the log's last line.
- The last lines of a truncated log are the marker, not the build's final output: the failure summary (0112) ends with the marker.
- Runner backpressure is unchanged: a truncated attempt is acknowledged at the same pace as one that is stored.

## Rejected alternatives

- Failing or cancelling the attempt at the bound: a verbose but correct build would fail for its verbosity.
- Keeping the last 64 MiB instead of the first: needs rewriting the stored file and offsets already served to watchers.
- A configurable bound: no deployment asked for one.
