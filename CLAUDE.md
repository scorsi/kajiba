# kajiba — notes for AI assistants

Read [`docs/adr/`](docs/adr/) before making an architectural choice; each ADR
states what was decided and why. [`docs/milestones.md`](docs/milestones.md)
is the plan — acceptance criteria there are the definition of done for a
milestone, not a suggestion.

## Conventions

- One version (`VERSION` at the repo root) across the Cargo workspace, the Go
  CLI and `ui`'s `package.json`. `nix flake check`'s
  `versions-identical` check enforces it (0023).
- `proto/` is the only place `.proto` contracts are defined. Regenerate after
  editing: `cd proto && buf generate` (Go and TypeScript; needs `protoc-gen-go`,
  `protoc-gen-go-grpc` and `apps/ui/node_modules/.bin` on `PATH` — all
  provided by `nix develop`). The Rust client is generated at `cargo build`
  time by `crates/kajiba-proto/build.rs` (tonic-build), not through `buf`.
  Generated Go and TypeScript code is committed; generated Rust code is not.
- Crate and module names are fixed by ADR 0002/0018: `kajiba-core`,
  `kajiba-git`, `kajiba-proto`, `kajiba-exec`, `kajiba-server`,
  `kajiba-runner`, `kajiba-cli`, `kajiba-ui`. Don't introduce new
  top-level crates without an ADR.
- Format with `nix fmt` (treefmt: rustfmt, gofmt, prettier, nixfmt). Generated
  code and lockfiles are excluded.
- Every milestone's acceptance criteria are proven by `nix flake check`
  (unit + `nixosTest` integration) or, from J5, Playwright e2e. If a change
  can't be verified that way, say so instead of claiming it works.
