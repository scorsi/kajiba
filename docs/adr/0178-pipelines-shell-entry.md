# ADR 0178 — Pipelines shell entry

Status: Accepted — 2026-11-01. Amends 0150.

## Context

The owner expects run pages to select Pipelines in the shell overview.

## Decision

Add a Pipelines entry linking to Activity's open Pipelines band
(`/?pipelines=1#pipelines`). Run routes select that entry instead of Activity.
Activity itself continues to select Activity. No new route is introduced.

## Consequences

The shell's current-entry tests cover both destinations independently.
