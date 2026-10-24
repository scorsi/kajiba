# ADR 0003 — Git foundation: library for reads, git binary for transport, hooks

Status: Accepted — 2026-10-09 (hooks confirmed on 2026-10-11; push durability and local socket accepted 2026-10-24)

## Context

No library (gix, libgit2, go-git) provides a complete server side of the smart protocol. A home-grown implementation is a possible pleasure but without feedback for a long time. The `pre-receive`/`post-receive` hooks must enforce permissions and emit events without duplicating the server's logic.

## Decision

- Transport (clone, fetch, push): spawn `git upload-pack` / `git receive-pack` (SSH) and `git upload-pack --stateless-rpc` (HTTP, `info/refs` and `git-upload-pack` routes). No `http-backend` CGI.
- Reads for the UI and the API (trees, blobs, refs, log, diff, blame): the **gix** library (gitoxide), pure Rust. Free to change during the first implementation if it doesn't keep its promises.
- Every interaction with git goes through an internal `GitStore` boundary (trait): `gix + git binary` implementation today, pieces replaceable one by one with home-grown code later.
- **Hooks**: the hook is the server binary itself, `kajiba-server hook <pre-receive|post-receive>`. It reads stdin (ref updates) and the environment set by `serve-ssh` (user, repo), then makes **one gRPC call over the server's local socket**: `HookService.PreReceive` returns allowed/denied with per-ref reasons; `HookService.PostReceive` notifies completion of ref updates for durable `push` event conversion (0017). No logic in the hook.
- **Local socket**: `/run/kajiba/local.sock`, owner `kajiba`, mode 0600, created by the server; it carries `kajiba.hook.v1` and `kajiba.admin.v1` (0014, 0016, 0021). Only the `kajiba` user (hooks) and root (admin commands) can connect.
- **Push durability**: before returning allowed, `PreReceive` commits a `pending_push` row (principal, repo, correlation id, ordered per-ref old/new sha). Admission is serialized per repository: its next push is refused with an explicit retry message until the admitted `receive-pack` has exited and all earlier pending rows have been reconciled. This gate survives server restarts: surviving `receive-pack` processes must finish or be stopped before reconciliation; row age alone is not proof of completion. Other ref mutations use the same gate.
- **Reconciliation**: `PostReceive` wakes reconciliation and returns without waiting for `receive-pack` to exit. Normal completion and recovery use the same ordered conversion of `pending_push` rows into `push` events. Once `receive-pack` has exited, compare each recorded ref with its old/new sha: new means applied (emit one event); old means not applied (no event); absent matches the zero sha for creation/deletion. Ignore old = new entries. An unexpected value retains the row and blocks further ref mutations with an explicit error; never silently drop evidence. Append events for the applied refs in recorded order and delete the pending row in one SQLite transaction. Run recovery at startup, on push completion and on a timer. This covers partial success, deletion and a crash after ref update but before `PostReceive`: every applied ref update has exactly one durable event before a later push can advance it. The guarantee assumes durable Git and SQLite storage and no out-of-band ref writes; it is not an atomic transaction across Git and SQLite.
- No per-repository hook file: a global `core.hooksPath` (git config set by the NixOS module) points to a Nix store directory containing links to the hook.

## Consequences

- Protocol reliability and correctness delegated to git; one process per request, negligible at this scale.
- The `git` binary becomes a runtime dependency of the NixOS module.
- The server must be running to accept a push; a push during a restart is cleanly refused (explicit message) and replayed by the user.
- If gix lacks an operation, one-off fallback to the git binary behind the same boundary, or git2 if necessary.

## Rejected alternatives

- Everything through the git binary: simple but output parsing all over the UI.
- git2 / libgit2: more complete on some points, but a C dependency and less active.
- Home-grown implementation from the start: too long without feedback; still possible piece by piece thanks to `GitStore`.
- Hooks in bash, Nim or nushell: one more toolchain and a gRPC client to write for three lines; a hook reading the DB directly would duplicate the logic and create a second SQLite writer.
