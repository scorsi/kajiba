# ADR 0158 — Runner occupancy in administration

Status: Accepted — 2026-11-01. Amends 0144.

## Context

A runner can execute several attempts up to its declared `max_jobs`. The
administration API already returns that capacity and every running attempt,
but the browser kept only the oldest attempt.

## Decision

- Preserve every current attempt and `max_jobs` at the UI's `api.ts` boundary.
- Administration shows occupied slots over capacity, distinguishing idle,
  partially busy, full and offline. Disconnection takes precedence over occupancy;
  attempts still reported as running remain visible until the server ends them.
- Show one row per running attempt with its run link, repository, ref or worktree
  label and attempt start time. Reuse the run's worktree identity (0151).
- Keep runners out of the shell overview, as decided in 0150. The runner named
  on an individual run page describes that run, not the runner's entire workload.

## Consequences

No protocol, configuration, dependency or scheduling changes. Server tests prove
that all running attempts are returned and completed attempts disappear; UI tests
cover occupancy and the administration flow with a two-slot runner.
