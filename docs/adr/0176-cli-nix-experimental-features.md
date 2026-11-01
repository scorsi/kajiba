# ADR 0176 — CLI Nix experimental features

Status: Accepted — 2026-11-01. Amends 0097.

## Context

The local evaluation for `kajiba ci run --worktree` uses the Nix command and
flakes, but fails on hosts whose `nix.conf` does not enable those features.

## Decision

Every CLI invocation of Nix passes `--extra-experimental-features` with the
single argument `nix-command flakes`. This includes the local evaluation of
`checks.<system>` for worktree requirements (0097).

## Consequences

Worktree evaluation does not require users to enable these features in their
Nix configuration. Existing experimental features remain enabled; no host
configuration, dependency or CLI option changes.
