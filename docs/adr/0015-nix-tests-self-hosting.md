# ADR 0015 — Nix: flake outputs, NixOS module, tests, self-hosting

Status: Accepted — 2026-10-09 (revised 2026-10-24: Rust CLI, no flakeModule, test gate, validators)

## Context

Everything is managed by Nix. `nix flake check` must run unit and integration tests. On aarch64-darwin only the non-VM checks are realistic; the Linux CI does the rest. A deployment may consume kajiba as a flake input served by kajiba itself.

## Decision

- Outputs: `packages.{kajiba-server,kajiba-runner,kajiba-cli,kajiba-ui}`; `nixosModules.server` (`services.kajiba-server`: hardened systemd unit `kajiba-server.service`, fixed system user and group `kajiba`, `stateDir` and paths to persist, dedicated sshd instance, `core.hooksPath`, timers, secret-file options: mirror credentials, runner tokens), `nixosModules.runner` (`services.kajiba-runner`, unit `kajiba-runner.service`, user `kajiba-runner`) and `darwinModules.runner` (0008); `devShells.default` (Rust, pnpm, buf, protoc, sqlx-cli, mold, playwright); `apps.e2e` (J4); `checks`; `formatter`.
- **No flakeModule export**: consumers import the modules and packages directly; kajiba uses flake-parts internally only.
- Builds: crane (Rust: server, runner, CLI), `fetchPnpmDeps` (frontend), protos compiled within the builds; the frontend bundle is injected into the server build (0006).
- Checks by tier:
  - **Unit**: workspace tests and clippy (crane), builds of every package, UI type check, proto lint and generated-code freshness (0020), `versions-identical` (0023); all systems.
  - **Integration**: `nixosTest` (Linux only, `lib.optionalAttrs`) with nodes `forge`, `runner`, `remote` (bare repo behind sshd, simulating GitHub or another forge) and, where needed, `proxy` (TLS-terminating reverse proxy). Scenarios per milestone. Every nixosTest that starts a kajiba service runs `nisshi validate --logs` on its journal and, when metrics are enabled, `masu validate --metrics` (0022); nisshi and masu are **test-only** flake inputs.
  - **E2E** (from J4): Playwright against a real instance, `nix run .#e2e` (starts a server from the flake, runs the suite), executed by the CI outside the Nix sandbox.
- **Gate**: a milestone closes when Linux `nix flake check` and the `e2e` job are green. Checks that depend on a real deployment are not acceptance criteria of the product; they live in the deployment repository.
- **Self-hosting**: kajiba can host its own repository and run its own CI and deployment (J12). A deployment that takes kajiba (or any flake) from kajiba keeps a fallback source with the same content — the GitHub mirror gives the same `narHash` — because the forge can be down while its own deployment is being repaired. A pipeline that redeploys the forge tolerates the service restart (0008).

## Consequences

- The full `nix flake check` is only green on Linux; on Darwin it is partial by construction, documented.

## Rejected alternatives

- E2E required in the Nix sandbox from v0: browsers and Playwright in the sandbox are fragile; possible later.
- Exporting a flakeModule: no consumer needs more than the modules and packages.
