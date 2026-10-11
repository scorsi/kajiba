# Contributing

This is a personal project (see ADR 0001) developed in the open. External
contributions aren't accepted yet — the merge workflow described in ADR 0014
(open) and milestone J16 doesn't exist. Issues and discussion are welcome.

## Development environment

Everything is managed by Nix:

```sh
nix develop
```

gives you Rust (stable), Go, pnpm, buf, protoc, sqlx-cli and mold. No other
toolchain is supported.

## Checks

```sh
nix flake check   # unit tests + nixosTest integration (Linux only)
nix fmt            # format the whole repo (treefmt)
```

`nix flake check` is partial on `aarch64-darwin`: the `nixosTest` integration
check only runs on Linux, by construction (NixOS VMs aren't cross-built from
Darwin). CI runs the full check on `aarch64-linux`.

## Protobuf

`.proto` files live in `proto/`, one module, package `kajiba.v1`. After
editing a `.proto` file:

```sh
cd proto && buf lint && buf generate
```

This regenerates the Go client (`apps/cli/internal/genproto/`) and the
TypeScript client (`apps/ui/src/lib/gen/`); both are committed. The Rust client isn't
touched by `buf generate` — it's generated fresh on every `cargo build` by
`crates/kajiba-proto/build.rs`.

## Versioning

One semver version, in `VERSION` at the repo root, mirrored in
`Cargo.toml`'s `[workspace.package].version`,
`apps/cli/internal/version/version.go` and `apps/ui/package.json`. The
`versions-identical` check fails if any of
them drift (ADR 0023).
