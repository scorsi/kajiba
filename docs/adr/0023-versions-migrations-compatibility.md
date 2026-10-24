# ADR 0023 — Versions, migrations and compatibility

Status: Accepted — 2026-10-11 (revised 2026-10-24: no Go module; version matrix and newer-schema refusal accepted)

## Context

Monorepo (0018): server, runner and CLI share one version. The git history must stay free (merge commits, non-linear releases). The SQLite schema evolves.

## Decision

- **Single semver version** in `VERSION` at the repository root, carried by the Cargo workspace and `apps/ui/package.json` (checked identical by `versions-identical`); Nix packages read it from `VERSION`. A release = one commit carrying the bump (typically a merge commit) + a **signed git tag** `vX.Y.Z` on that commit. No linearity constraint between releases; the release method (branch, script, changelog) will be specified later.
- **Forward-only schema migrations**, numbered, embedded, applied at startup under the single-instance lock (0012). No downgrade: rolling back is "restore the last dump" (0016), tested in `nixosTest`. A database with a migration the binary does not know is refused at startup. Door left open to down migrations or a maintenance mode later.
- **Compatibility matrix**. The runner announces its version on connection, the CLI on every call (0007).

  | Pair            | Before 1.0.0                              | From 1.0.0                                                         |
  | --------------- | ----------------------------------------- | ------------------------------------------------------------------ |
  | server ↔ runner | same `0.minor` required, patch may differ | same major required, minor and patch may differ                    |
  | server ↔ CLI    | same `0.minor` required, patch may differ | same major required; a different minor prints a warning on the CLI |

  Incompatible = refused with `FAILED_PRECONDITION`, reason `VERSION_INCOMPATIBLE`, naming both versions (0020).

## Consequences

- A deployment building server and runners from the same flake revision is always compatible; the rule matters during redeploy windows and for runners updated separately.
- A proto contract break after 1.0.0 implies a major bump (0020).

## Rejected alternatives

- Independent versions per component: drift to manage, contrary to 0018.
- Mandatory reversible migrations: permanent cost for a rare case.
- Tolerating a different `0.minor` with a warning: before 1.0 a minor may break the protocol (0020).
