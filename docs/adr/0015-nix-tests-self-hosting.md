# ADR 0015 — Nix: flake outputs, NixOS module, tests, self-hosting

Status: Accepted — 2026-10-09

## Context

Everything is managed by Nix. `nix flake check` must run unit, integration and e2e tests. Locally (aarch64-darwin) only unit tests are realistic; the Linux CI does the rest. The forge will be a sekkeizu input served by itself.

## Decision

- Outputs: `packages.{kajiba-server,kajiba-runner,kajiba-cli,kajiba-ui}`, `nixosModules.server` (hardened systemd service, `kajiba` user, `stateDir` and paths to persist, dedicated sshd instance, `core.hooksPath`, cleanup and reconciliation timers, sops options: mirror secrets, runner tokens), `nixosModules.runner` and `darwinModules.runner` (0008), `devShells.default` (Rust, Go, pnpm, protoc, sqlx-cli, mold), `checks`, `formatter`.
- Builds: crane (Rust), `buildGoModule` (CLI), `fetchPnpmDeps` (frontend), protos compiled within the builds; the frontend bundle is injected into the server build (0006).
- Checks by tier:
  - **Unit**: within the package builds, all systems.
  - **Integration**: `nixosTest` (Linux only, `lib.optionalAttrs`) with three nodes — `forge`, `runner`, `remote` (bare repo behind sshd, simulating GitHub) — scenarios: HTTP clone, SSH push, refusal of a ref outside the prefix, refusal of an unsigned commit, `nix` job executed and logs received, mirror synchronized then recovered after a `remote` outage, `forge` restart during a job.
  - **E2E**: Playwright against a real instance started by the CI outside the sandbox (like rirekisho), or in a nixosTest node if the sandbox allows it painlessly.
- Self-hosting: `scorsi-kajiba` input served by kajiba, **GitHub fallback kept** (`via-github.nu`, identical narHash); the pipeline that deploys the forge on ishizue tolerates the service restart (0008).

## Consequences

- The full `nix flake check` is only green on Linux; locally, `nix flake check` is partial by construction, documented.
- The kiso repository provides the flake base; kajiba exports a flakeModule like the others.

## Rejected alternatives

- E2E required in the Nix sandbox from v0: browsers and Playwright in the sandbox are fragile; possible later.
