# ADR 0155 — Runner state serialization dependency

Status: Accepted — 2026-11-01. Amends 0034.

## Context

Z1 persists runner attempt records across process restarts. The runner already uses
`serde_json`; typed records need `serde` derives.

## Decision

Make workspace `serde` a direct dependency of `kajiba-runner`. It is already used
by `kajiba-core` and present in `Cargo.lock`; no new package or feature is added.

## Consequences

The Cargo dependency graph changes; npm dependencies and the pnpm dependency hash
do not change.
