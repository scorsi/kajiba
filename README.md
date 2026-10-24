# kajiba (鍛冶場, "the forge")

A self-hosted git forge: repository hosting over SSH and HTTP, CI through a
small runner protocol with interchangeable execution backends, one-way
mirroring to GitHub, a web UI, and a CLI. One owner to start, built to grow
into a small team without a rewrite; deployed through Nix only. See [`docs/adr/`](docs/adr/) for the
design decisions and [`docs/milestones.md`](docs/milestones.md) for the plan.

Status: **J0** — skeleton only, no feature works yet. J1 starts by replacing
the Go CLI skeleton with a Rust crate (ADR 0007); until then `apps/cli` is
still a Go module.

## Layout

- `proto/` — `.proto` files, the source of truth for the API and the
  forge↔runner protocol.
- `crates/` — shared Rust libraries (no binary): `kajiba-core`, `kajiba-git`,
  `kajiba-proto`, `kajiba-exec`.
- `apps/` — the four deliverables, one subfolder each:
  - `server/` — `kajiba-server`, the Rust binary.
  - `runner/` — `kajiba-runner`, the Rust binary.
  - `cli/` — `kajiba-cli`, the Rust CLI (binary `kajiba`).
  - `ui/` — SvelteKit SPA, `kajiba-ui`, embedded into `kajiba-server` at
    build time from J5 on.
- `nix/` — packages, NixOS/nix-darwin modules, tests.
- `docs/adr/` — architecture decision records.
- `pnpm-workspace.yaml` — workspace marker at the repo root for `apps/ui`
  (one member; mostly so that tools that detect a project by its manifest
  file see Node from the repo root).

## Building

```sh
nix develop            # devShell: Rust, pnpm, buf, protoc, sqlx-cli, mold
nix flake check        # unit + integration checks
nix build .#kajiba-server .#kajiba-runner .#kajiba-cli .#kajiba-ui
```

`nix flake check` is only complete on Linux (`aarch64-linux`); the NixOS
integration tests don't run on Darwin.

## License

Apache-2.0, see [`LICENSE`](LICENSE).
