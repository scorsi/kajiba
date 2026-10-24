# ADR 0009 — Interchangeable execution backends: nix → nspawn → microvm

Status: Accepted — 2026-10-09 (revised 2026-10-24: boundary of the `nix` backend)

## Context

A security boundary is wanted from the start, but fast feedback comes first. Hosts vary: some cannot run nested virtualization; a macOS runner has no Linux isolation.

## Decision

- `Executor` trait in `kajiba-exec`: `prepare(job) → env`, `run(step) → log stream + exit code`, `cleanup`. Each backend is an implementation; the runner announces the ones it can provide (0008).
- Delivery order:
  1. **`nix`**: the job is `nix build` / `nix flake check` on the checked-out revision. **Boundary: derivations run in the Nix build sandbox (no network except fixed-output derivations, no secrets). Evaluation and input fetching run as the runner user, with network.** Requirements on the runner host, checked by the runner at startup: `sandbox = true`, the runner user is not in `trusted-users`, `accept-flake-config = false` (a repository's `nixConfig` cannot weaken the sandbox), the runner's token file is not readable by build users. Evaluation stays in pure mode (no access to arbitrary host paths). No secret is ever passed to a `nix` job. **Whole-job isolation** (network cut during evaluation, resource limits) is not this backend's promise: it is `nspawn`'s or `microvm`'s.
  2. **`nspawn`**: `systemd-nspawn` container (or sandboxed `systemd-run`) from a Nix-built root, network and secrets injected, ephemeral files.
  3. **`microvm`**: microvm.nix (cloud-hypervisor/firecracker) on hosts that support virtualization; a spike on the target host before any commitment.
  - **`process`**: direct execution, no isolation, reserved for trusted jobs; the only backend on macOS.
- CI secrets: outside the forge at first (files provided to the runner host by its deployment, exposed to the backends that support them); managed by the forge later.

## Consequences

- v0 contains no container machinery: a job = one Nix command.
- A job requiring an absent backend stays pending and says so clearly.
- A repository's flake can make the runner fetch anything during evaluation; this is accepted for trusted refs (0013) and is why untrusted refs never trigger CI automatically.

## Rejected alternatives

- podman/OCI: portable and familiar, but more machinery and less Nix-native; possible as an additional backend later.
- microVM from the start: not available on every host.
- Cutting the network for the whole `nix` job: evaluation needs to fetch flake inputs; doing it offline means pre-fetching inputs outside the job, which is the `nspawn` backend's job.
