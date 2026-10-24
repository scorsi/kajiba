# ADR 0018 — Code organization: monorepo

Status: Accepted — 2026-10-09 (revised 2026-10-24: the CLI is a Rust crate, 0007)

## Context

Four components (server, runner, CLI, frontend) in two languages (Rust, TypeScript) plus a shared gRPC schema. Question: one repository per component or a single one.

## Decision

- **A single `kajiba` repository**:
  - `proto/` — `.proto` files, source of truth (0005, 0008), compiled by each build.
  - `crates/` — shared Rust libraries, no binary: `kajiba-core`, `kajiba-git`, `kajiba-proto`, `kajiba-exec`.
  - `apps/` — the four deliverables, one subfolder each:
    - `server/` — `kajiba-server` binary.
    - `runner/` — `kajiba-runner` binary.
    - `cli/` — `kajiba-cli` crate, binary `kajiba`.
    - `ui/` — SvelteKit project (pnpm), `kajiba-ui` component.
  - `nix/` — packages, NixOS/nix-darwin modules, nixosTest tests.
  - `docs/adr/`, `docs/milestones.md`, `docs/schema.md`.
  - `flake.nix`, `Cargo.toml` (workspace), `pnpm-workspace.yaml` at the root (one member; mostly so that tooling that detects a project by its manifest file sees Node from the root), a single devShell.
- A cross-cutting change (proto + server + runner + CLI) fits in one commit and a single `nix flake check`.
- Single versioning: the server, the runner and the CLI carry the same version; protocol compatibility is guaranteed by construction when they are built from the same revision, and by 0023's rule otherwise.

## Consequences

- No registry or submodule for the protos; no version drift to manage.
- A single circular bootstrap (0015) instead of four.
- Nix builds remain separate derivations per component: a monorepo doesn't imply a mono-build.

## Rejected alternatives

- One repository per component: only for truly independent life cycles; here it would be four flakes, four CIs, proto sharing to tool and versions to synchronize.
