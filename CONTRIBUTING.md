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

### Web application (`pnpm dev`)

```sh
cd apps/ui && pnpm dev   # http://localhost:5173, hot reload, no Rust rebuild
```

Vite proxies grpc-web (`/kajiba.v1.*`) and `/auth/webauthn/*` to the server
named by `KAJIBA_DEV_URL` (default `http://127.0.0.1:8080`), so the browser
sees one origin (ADRs 0006, 0102):

- **Local server**: run it with `http.public_url = "http://localhost:5173"`
  (the Vite origin) and open that URL; passkey enrollment and login work as in
  production.
- **Remote instance**: passkeys cannot work there (the relying party is the
  remote host). Log in with the CLI into a file of your own, then point Vite
  at it; the proxy sends that PAT and drops the browser's `Origin` and
  cookies:

  ```sh
  kajiba login --url https://forge.example.org --token-file ~/.config/kajiba/dev
  KAJIBA_DEV_URL=https://forge.example.org KAJIBA_DEV_TOKEN_FILE=~/.config/kajiba/dev pnpm dev
  ```

  The file's URL must be `KAJIBA_DEV_URL`, and a token goes only over HTTPS
  (or plain HTTP to loopback), as with the CLI (ADRs 0098, 0099).

## Checks

```sh
nix flake check   # unit tests + nixosTest integration (Linux only)
nix fmt            # format the whole repo (treefmt)
```

`nix flake check` is partial on `aarch64-darwin`: the `nixosTest` integration
checks only run on Linux, by construction (NixOS VMs aren't cross-built from
Darwin). CI runs the full check on `aarch64-linux`, plus the Playwright `e2e`
job from J4 (ADR 0015).

### `e2e` job

```sh
nix run .#e2e                      # whole suite
nix run .#e2e -- --grep enrollment # Playwright arguments pass through
```

The app starts `kajiba-git` and `kajiba-server` from the flake's packages as
the current user, in a temporary directory under `/tmp` (socket directories
and peer identities set in a generated configuration, ADRs 0093 and 0095),
with `http.public_url = "http://localhost:<port>"` and `auth.session_ttl =
"1m"`. The server serves `packages.kajiba-ui` through `--ui-dir` (ADR 0094).
The Playwright suite of `apps/ui/e2e/` then runs with nixpkgs' Chromium
(`playwright-driver.browsers`, matching the pinned `@playwright/test`);
Playwright never downloads a browser. The app exits with the suite's status
and prints both daemons' last log lines on failure.

- `KAJIBA_E2E_PORT` (default `18080`): the server's port, which must be free.
- `KAJIBA_E2E_OUTPUT` (default `./e2e-results`): traces of failed tests.

It runs outside the Nix sandbox and is never part of `nix flake check`. The
CI job runs it on Linux after the flake check; it needs no root, no `/run`
directory and no VM.

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

`buf-breaking` compares `proto/` with the last release's `proto/`, pinned as
the `proto-baseline` flake input (ADR 0101). Until the first release there is
no such input: the check passes and prints `no release baseline`. With each
release, point the input at this repository at the release tag, then
`nix flake update proto-baseline`:

```nix
proto-baseline = {
  url = "git+https://example.org/kajiba.git?ref=refs/tags/v<version>";
  flake = false;
};
```

The check also exercises itself on temporary copies of `proto/`:

```sh
bash nix/checks/buf-breaking-test.sh "$PWD/nix/checks/buf-breaking.sh" "$PWD/proto"
```

Before 1.0.0, breaking changes are allowed in a minor bump and the check only
reports them; from 1.0.0 on, it fails on them (ADRs 0020, 0023).

## Versioning

One semver version, in `VERSION` at the repo root, mirrored in
`Cargo.toml`'s `[workspace.package].version` and `apps/ui/package.json`. The
`versions-identical` check fails if they drift and requires `kajiba-git` to
inherit the workspace version (ADRs 0023, 0054).
