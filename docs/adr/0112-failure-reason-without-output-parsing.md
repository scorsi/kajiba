# ADR 0112 — Failure reason without parsing build output

Status: Accepted — 2026-10-31. Amends 0009, 0010.

## Context

A failed run is summarized to its reader (CLI and UI) by the failing check, the reason and the last log lines. The `nix` backend runs `nix flake check` as one step (0105): Nix reports which check failed only in its human-readable output, whose wording changes between Nix versions. The owner decided on 2026-10-30 how the backend fills these fields.

## Decision

- **Amends 0009 — what a backend reports.** A step's result is its exit: an exit code, or the signal that killed it, or a cancellation. The reason of a failed attempt is derived from that exit alone (`exit status <n>`, `killed by signal <n>`) or from the runner's own error when the step could not run (sources unavailable, program missing).
- **Amends 0010 — the failing check is best-effort.** The failing check's name is filled only when the backend knows it structurally, for example when a run builds selected checks one by one; for a whole `nix flake check` it stays empty. No backend parses a tool's human-readable output to guess it.
- The summary's last log lines are always available: they are the attempt's stored log.

## Consequences

- A failure summary always carries a reason and log lines; the reader finds the failing check's name in those lines when the field is empty.
- A Nix upgrade cannot break the summary.

## Rejected alternatives

- Parsing Nix's `error: builder for '…' failed` lines: brittle across Nix versions, and a wrong guess is worse than none.
- `--log-format internal-json` to read the failing derivation: maps a derivation, not a check attribute, and doubles the log volume the runner sends.
