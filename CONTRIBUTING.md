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
other toolchain is supported.

The shared GitStore library is `kajiba-gitstore` in `crates/kajiba-gitstore/`
(ADRs 0002, 0018 and 0054). The git-side app is `kajiba-git` in `apps/git/`,
also the binary and Nix package name. It depends on `kajiba-gitstore` and
shares the workspace's version and Cargo dependency artifacts. Its commands
are `kajiba-git --version`, `kajiba-git config check <path>` and
`kajiba-git serve --config <path>`; the daemon runtime is added in J3 task 9.

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
pnpm install --frozen-lockfile # install UI tools first, from the repository root
cd proto && buf lint && buf generate
```

This regenerates the TypeScript client (`apps/ui/src/lib/gen/`), which is
committed. The Rust code isn't touched by `buf generate` — it's generated
fresh on every `cargo build` by `crates/kajiba-proto/build.rs`, for the
server, the runner and the CLI.

The devShell adds `apps/ui/node_modules/.bin` to `PATH` when entered from
the repository root. `proto-generated` uses the same pnpm dependencies in
the sandbox and compares a fresh generation with the committed directory,
including missing and extra files. Run the same check locally from the root:

```sh
bash nix/checks/proto-generated.sh
bash nix/checks/proto-generated-test.sh "$PWD/nix/checks/proto-generated.sh"
```

`buf-breaking` is informational: in `nix flake check` it always passes with a
skip message, because the Nix sandbox source has no git history (no `.git`, no
tags) to compare against. Run the comparison manually from a checkout, against
the last release tag (replace `<tag>` with that tag):

```sh
cd proto
buf breaking --against '.git#tag=<tag>'
```

Before 1.0.0, breaking changes are allowed in a minor bump (ADRs 0020, 0023).

## Versioning

One semver version, in `VERSION` at the repo root, mirrored in
`Cargo.toml`'s `[workspace.package].version` and `apps/ui/package.json`. The
`versions-identical` check fails if they drift and requires `kajiba-git` to
inherit the workspace version (ADRs 0023, 0054).
