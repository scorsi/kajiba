# ADR 0010 — Pipelines: Nix convention with no file at first, never Actions compatibility

Status: Accepted — 2026-10-09 (revised 2026-10-24: external Nix cache)

## Context

Repositories hosted on the forge are expected to be flakes with `checks`; there is no existing workflow format to preserve.

## Decision

- v0: **no pipeline file**. Convention: a push on a trusted ref (0013) of a repository with CI enabled creates a `nix flake check` run (`nix` backend). The job targets the system of the matched runner (`aarch64-linux` first).
- Manual trigger: "run on this sha" through UI/CLI, for users with the required scope; it is also the only path for an external contribution (0013, 0014).
- Planned evolution: a dedicated `.kajiba/pipeline.toml` file (jobs = command + `needs` + runner requirements + minimal matrix) when impure steps require it — for example deploying a static site or the forge itself. GitHub Actions compatibility: definitively excluded.
- Artifacts and logs: short retention (`ci.log_retention`, `ci.artifact_retention`, 0019), cleanup by timer.
- **Nix binary cache** (external, 0001): a runner host that should publish its build outputs configures Nix itself — a signing key and a `post-build-hook` that uploads to the cache (e.g. oroshi). kajiba ships no upload code and no cache credential. **CI artifacts** (files a job declares, stored by the forge with retention) and **cache objects** (store paths, held by the cache) are distinct and never converted into each other. A store path's content is bound to its derivation, so uploading outputs of a manually triggered untrusted build cannot replace a trusted path, given 0009's runner-host requirements.

## Consequences

- The `run/job/step` model (0013) exists from v0 even if each run has a single job; the file format plugs into it without a redesign.
- Impure steps wait for the pipeline file and the `nspawn` backend (0009).

## Rejected alternatives

- Actions compatibility: huge surface (expressions, marketplace, Node) with nothing to migrate.
- Pipeline declared in the flake (`outputs.ci`): consistent but ties CI to Nix evaluation, slow on small hosts; still possible as an alternative source of the file later.
- Cache upload as a forge feature: credentials in the forge and a second path for the same bytes; Nix's own hook does it.
