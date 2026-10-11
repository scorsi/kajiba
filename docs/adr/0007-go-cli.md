# ADR 0007 — CLI: Go, client generated from the protos

Status: Accepted — 2026-10-09

## Context

Initial uses: create a repo, trigger and follow a CI run. Targets: macOS and Linux (NixOS), aarch64 first, x86_64 next. Windows: unplanned nice-to-have. The API is gRPC (0005).

## Decision

- `kajiba-cli` in **Go**: grpc-go client generated from the same `.proto` files, cobra for commands, text output and `--json`.
- Distributed through Nix (`packages.kajiba-cli`, integrated into kiso/home-manager like the other tools). Cross-compilation for other OSes possible later without effort (`GOOS`/`GOARCH`).
- Authentication: personal access token (PAT, 0014) stored in the OS config file; device flow against the forge later.

## Consequences

- Multi-language justified by the shared schema; the devShell carries Go and Rust.
- No business logic in the CLI: it is a client.

## Rejected alternatives

- Rust: feasible (clap + tonic), no clear advantage for a client.
- Nim, Zig: no mature gRPC client; Zig is better suited to low-level code.
