# ADR 0188 — Gc timer clear of the dump

Status: Accepted — 2026-11-02. Amends 0173.

## Context

`kajiba-gc.timer` fires `weekly`, Monday 00:00, together with the daily
`kajiba-dump.timer`. The dump holds maintenance first, the gc is refused with
`maintenance in progress; retry later` and, by 0173, is not retried: in
production the weekly gc never ran.

## Decision

- `kajiba-gc.timer` fires `OnCalendar=Mon 04:00`, still weekly and persistent,
  after the daily dump and the 03:00 CI retention have finished.
- No retry loop, ordering dependency or new option: the two timers simply do not
  share a minute. A deployment moving one timer keeps them apart itself.

## Consequences

The module check expects the new calendar. Nothing else changes.
