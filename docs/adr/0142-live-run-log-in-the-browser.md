# ADR 0142 — Live run log in the browser

Status: Accepted — 2026-10-31.

## Context

The run page (0086) showed the stored log of a run, read once with `WatchRun` (`follow` false), so a running run needed a reload to move. `WatchRun` (0120, 0129) already streams the stored bytes, then the live ones and the terminal run; the browser only has to consume it.

## Decision

- **One stream per open run page, while the run is not terminal.** The page reads the stored log, then, if the run is queued or running, calls `WatchRun` with `follow` from the received offset. Terminal runs open no following stream. Leaving the page (client-side navigation, closing the tab) aborts the stream, so a tab never holds more than one of the caller's 16 watches (0120).
- **Resume.** A broken stream, or one the server ends without the final state (a stopping server, 0140), is resumed from the received byte offset after a backoff (0.5 s doubling to 8 s), up to 5 consecutive failures; `NOT_FOUND`, `PERMISSION_DENIED`, `UNAUTHENTICATED` and `RESOURCE_EXHAUSTED` are not retried. Bytes overlapping what was received are dropped; a repeated attempt section does not start a new column. Keepalives are ignored.
- **Decoding.** One streaming UTF-8 decoder per run, so chunks may split a character or a line; ANSI escape sequences are removed; a carriage-return redraw keeps its last state; an unfinished last line is shown as it is.
- **Display.** Each attempt (a replay is a new one) is a column; the first attempt of a queued run turns it to running. Updates reach the page at most every 100 ms. A running console follows the end of the log unless the reader scrolled up; a finished one lands on the linked line (`#L<attempt>-<n>`), else the first `error:` line. When the final run arrives the page reloads its data once (history, failure summary, durations).

## Consequences

- No server change; the browser relies on 0120's limits and 0129's keepalives.
- A run page left open on a stalled connection stops after five failed resumes and keeps the log received; reloading the page retries.

## Rejected alternatives

- Polling `GetRun` and the stored log: it repeats the whole log and lags the runner.
- A shared worker or one stream for several tabs: the limit of 16 per principal is enough for one stream per page.
