# ADR 0153 — Comparable run duration bars

Status: Accepted — 2026-11-01. Amends 0138, 0144, 0145.

## Context

The owner's J5.1-a feedback asks for run duration bars that can be compared
without their labels changing the available width, and shorter labels for
runs lasting an hour or more.

## Decision

Activity, repository Pipelines and Administration use the same duration bar:
a fixed-width track followed by a fixed-width, right-aligned label with
tabular numerals. The filled width is proportional to elapsed seconds on a
shared scale: the longest running run in Activity, the longest loaded run in
the repository, and the longest loaded run in Administration respectively.
A zero duration has no fill. Step-state indicators remain separate from
duration bars.

The shared compact duration formatter uses seconds below one minute,
minutes and seconds below one hour, and hours and whole minutes from one
hour onward (`1h53m`). Seconds are dropped, not rounded, from that threshold.
Log clocks retain their minute/second format.

## Consequences

Bars are comparable within a view; loading a different set of runs may
change the scale. No server field, statistics RPC or dependency is added.
