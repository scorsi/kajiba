# ADR 0009 — Interchangeable execution backends: nix → nspawn → microvm

Status: Accepted — 2026-10-09

## Context

A security boundary is wanted from the start, but fast feedback comes first. Initial host: NixOS VM under vfkit (nested virtualization to be verified). A macOS runner on jiban has no Linux isolation.

## Decision

- `Executor` trait in `kajiba-exec`: `prepare(job) → env`, `run(step) → log stream + exit code`, `cleanup`. Each backend is an implementation; the runner announces the ones it can provide (0008).
- Delivery order:
  1. **`nix`**: the job is `nix build` / `nix flake check`; isolation = Nix sandbox; no network, no secrets.
  2. **`nspawn`**: `systemd-nspawn` container (or sandboxed `systemd-run`) from a Nix-built root, network and secrets injected, ephemeral files.
  3. **`microvm`**: microvm.nix (cloud-hypervisor/firecracker) when the hardware allows it; nested virtualization spike in vfkit (macOS 15+, M3/M4) before any commitment.
  - **`process`**: direct execution, no isolation, reserved for trusted jobs; the only backend on macOS.
- CI secrets: outside the forge at first (injected into the runner by sops on the host side, exposed to the backends that support them); managed by the forge later.

## Consequences

- v0 contains no container machinery: a job = one Nix command.
- A job requiring an absent backend stays pending and says so clearly.

## Rejected alternatives

- podman/OCI: portable and familiar, but more machinery and less Nix-native; possible as an additional backend later.
- microVM from the start: blocked by the current host.
