# kajiba — notes for AI assistants

Read [`docs/adr/`](docs/adr/) before making an architectural choice; each ADR
states what was decided and why. [`docs/milestones.md`](docs/milestones.md)
is the plan — acceptance criteria there are the definition of done for a
milestone, not a suggestion. [`docs/schema.md`](docs/schema.md) is the
relational schema.

## Conventions

- The docs describe the product only: no host name, domain or value of a
  particular deployment. Use `example.org` and "the deployment repository".
- One version (`VERSION` at the repo root) across the Cargo workspace and
  `ui`'s `package.json`; Nix packages read `VERSION`. `nix flake check`'s
  `versions-identical` check enforces it (0023).
- Everything kajiba ships is Rust, except the UI (TypeScript). The CLI is the
  `kajiba-cli` crate, binary `kajiba` (0007); the host binaries are
  `kajiba-server` in `apps/server/` and `kajiba-git` in `apps/git/`
  (0002, 0054), each with its own host-local subcommands.
- `proto/` is the only place `.proto` contracts are defined. Regenerate after
  editing: `cd proto && buf generate` (TypeScript; needs
  `apps/ui/node_modules/.bin` on `PATH`, provided by `nix develop`). The Rust
  code (server and client) is generated at `cargo build` time by
  `crates/kajiba-proto/build.rs` (tonic-build), not through `buf`. Generated
  TypeScript is committed and checked fresh; generated Rust is not committed.
- Crate and module names are fixed by ADR 0002/0018/0054: `kajiba-core`,
  `kajiba-gitstore`, `kajiba-proto`, `kajiba-exec`, `kajiba-server`, `kajiba-git`,
  `kajiba-runner`, `kajiba-cli`, `kajiba-ui`. NixOS options
  `services.kajiba-server` / `services.kajiba-git` / `services.kajiba-runner`;
  the two host daemons use system users `kajiba-server` and `kajiba-git`
  (0015, 0054). Don't introduce new top-level crates without an ADR.
- Logs are nisshi schema v1 lines on stderr, metrics follow masu's
  convention on a loopback listener (0022); never log a secret.
- Format with `nix fmt` (treefmt: rustfmt, prettier, nixfmt). Generated
  code and lockfiles are excluded.
- Every milestone's acceptance criteria are proven by `nix flake check`
  (unit + `nixosTest` integration) or, from J4, the Playwright `e2e` job. If
  a change can't be verified that way, say so instead of claiming it works.
