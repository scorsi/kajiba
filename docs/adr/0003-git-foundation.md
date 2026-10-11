# ADR 0003 — Git foundation: library for reads, git binary for transport, hooks

Status: Accepted — 2026-10-09 (hooks confirmed on 2026-10-11)

## Context

No library (gix, libgit2, go-git) provides a complete server side of the smart protocol. A home-grown implementation is a possible pleasure but without feedback for a long time. The `pre-receive`/`post-receive` hooks must enforce permissions and emit events without duplicating the server's logic.

## Decision

- Transport (clone, fetch, push): spawn `git upload-pack` / `git receive-pack` (SSH) and `git upload-pack --stateless-rpc` (HTTP, `info/refs` and `git-upload-pack` routes). No `http-backend` CGI.
- Reads for the UI and the API (trees, blobs, refs, log, diff, blame): the **gix** library (gitoxide), pure Rust. Free to change during the first implementation if it doesn't keep its promises.
- Every interaction with git goes through an internal `GitStore` boundary (trait): `gix + git binary` implementation today, pieces replaceable one by one with home-grown code later.
- **Hooks**: the hook is the `kajiba` binary itself, subcommand `kajiba hook <pre-receive|post-receive>`. It reads stdin (ref updates) and the environment set by `serve-ssh` (user, repo), then makes **one gRPC call over a Unix socket** to the running server: `HookService.PreReceive` returns allowed/denied with per-ref reasons; `HookService.PostReceive` emits the `push` event (0017). No logic in the hook.
- No per-repository hook file: a global `core.hooksPath` (git config set by the NixOS module) points to a Nix store directory containing links to `kajiba hook`.

## Consequences

- Protocol reliability and correctness delegated to git; one process per request, negligible at this scale.
- The `git` binary becomes a runtime dependency of the NixOS module.
- The server must be running to accept a push; a push during a redeploy is cleanly refused (explicit message) and replayed by the user.
- If gix lacks an operation, one-off fallback to the git binary behind the same boundary, or git2 if necessary.

## Rejected alternatives

- Everything through the git binary: simple but output parsing all over the UI.
- git2 / libgit2: more complete on some points, but a C dependency and less active.
- Home-grown implementation from the start: too long without feedback; still possible piece by piece thanks to `GitStore`.
- Hooks in bash, Nim or nushell: one more toolchain and a gRPC client to write for three lines; a hook reading the DB directly would duplicate the logic and create a second SQLite writer.
