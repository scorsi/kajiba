# Contributing

kajiba is developed in the open by its author (see ADR 0001). External
contributions aren't accepted yet — the merge workflow described in ADR 0014
(open) and milestone J16 doesn't exist. Issues and discussion are welcome.

## Development environment

Everything is managed by Nix:

```sh
nix develop
```

gives you Rust (stable), pnpm, buf, protoc, sqlx-cli, mold and Playwright. No
other toolchain is supported. (Until J1 task 2 the devShell also carries Go
for the CLI skeleton, ADR 0007.)

## Checks

```sh
nix flake check   # unit tests + nixosTest integration (Linux only)
nix fmt            # format the whole repo (treefmt)
```

`nix flake check` is partial on `aarch64-darwin`: the `nixosTest` integration
checks only run on Linux, by construction (NixOS VMs aren't cross-built from
Darwin). CI runs the full check on `aarch64-linux`, plus the Playwright `e2e`
job from J4 (ADR 0015).

## Protobuf

`.proto` files live in `proto/`, packages `kajiba.v1`, `kajiba.runner.v1`,
`kajiba.hook.v1`, `kajiba.admin.v1` (ADR 0020). After editing a `.proto`
file:

```sh
cd proto && buf lint && buf generate
```

This regenerates the TypeScript client (`apps/ui/src/lib/gen/`), which is
committed. The Rust code isn't touched by `buf generate` — it's generated
fresh on every `cargo build` by `crates/kajiba-proto/build.rs`, for the
server, the runner and the CLI.

## Versioning

One semver version, in `VERSION` at the repo root, mirrored in
`Cargo.toml`'s `[workspace.package].version` and `apps/ui/package.json`. The
`versions-identical` check fails if they drift (ADR 0023).
