# ADR 0023 — Versions, migrations and compatibility

Status: Accepted — 2026-10-11

## Context

Monorepo (0018): server, runner and CLI share one version. The git history must stay free (merge commits, non-linear releases). The SQLite schema evolves.

## Decision

- **Single semver `0.x` version** carried by the repository (version of the Cargo workspace, the Go module and the `package.json`, checked identical by a check). A release = one commit carrying the bump (typically a merge commit) + a **signed git tag** `vX.Y.Z` on that commit. No linearity constraint between releases; the release method (branch, script, changelog) will be specified later.
- **Forward-only schema migrations**, numbered, embedded, applied at startup under a lock. No downgrade: rolling back is "restore the last dump" (0016), tested in `nixosTest`. Simplest for now; door left open to down migrations or a maintenance mode later.
- **Server ↔ runner compatibility**: the runner announces its version on connection. Same major version required; minor difference tolerated (for the duration of a `switch` restarting both). In `0.x`, minor plays the role of major. Incompatible version = connection refused with an explicit reason.
- CLI: same rule towards the server, warning (not refusal) on a minor difference.

## Consequences

- sekkeizu points to a single input, so server and runners are always deployed from the same commit; the compatibility rule only matters during redeploy windows.
- A proto contract break after going to production implies a major bump (0020).

## Rejected alternatives

- Independent versions per component: drift to manage, contrary to 0018.
- Mandatory reversible migrations: permanent cost for a rare case.
