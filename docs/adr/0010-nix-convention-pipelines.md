# ADR 0010 — Pipelines: Nix convention with no file at first, never Actions compatibility

Status: Accepted — 2026-10-09

## Context

No existing workflow to preserve. All the repositories concerned are flakes with `checks`.

## Decision

- v0: **no pipeline file**. Convention: a push on a trusted ref (0013) of a repository with CI enabled creates a `nix flake check` run (`nix` backend). The job targets the system of the matched runner (`aarch64-linux` first).
- Manual trigger: "run on this sha" through UI/CLI, for you only; it is also the only path for an external contribution (0013, 0014).
- Planned evolution: a dedicated `.kajiba/pipeline.toml` file (jobs = command + `needs` + runner requirements + minimal matrix) when impure steps require it — for example deploying rirekisho (Astro + Svelte, served by Caddy) or the forge itself. GitHub Actions compatibility: definitively excluded.
- Artifacts and logs: short retention (default value in the NixOS module), cleanup by timer.

## Consequences

- The `run/job/step` model (0013) exists from v0 even if each run has a single job; the file format plugs into it without a redesign.
- Impure steps wait for the pipeline file and the `nspawn` backend (0009).

## Rejected alternatives

- Actions compatibility: huge surface (expressions, marketplace, Node) with nothing to migrate.
- Pipeline declared in the flake (`outputs.ci`): consistent but ties CI to Nix evaluation, slow on a VM; still possible as an alternative source of the file later.
