# ADR 0161 — Git session hooks stay with their daemon generation

Status: Accepted — 2026-11-01. Amends 0053, 0093, 0157.

## Context

An admitted receive-pack can invoke its hooks after shutdown starts. Its drain
must not depend on a new connection being accepted by the next daemon generation:
systemd starts that generation only after the old one exits. The shared local RPC
listener already remains available during drain, but session hooks should have an
explicit channel to their supervisor, independent of socket activation.

## Decision

- The Git daemon binds `<run_dir>/session.sock` itself, without socket activation,
  mode 0600 and with the Git user's peer UID check. It serves only `HookService`,
  retaining the normal RPC correlation, session context and signature checks.
- Every supervised SSH Git child inherits `KAJIBA_SESSION_SOCKET` naming this
  socket. Both receive hooks use it instead of the default activated socket;
  connection failure fails closed without falling back to another generation.
  Hooks without this internal environment variable retain their existing default.
  No public configuration option, dependency or protobuf field is added.
- The socket belongs to the daemon generation for its entire lifetime. The state
  lock and systemd's cgroup cleanup prevent overlapping generations; its pathname
  may therefore be reused. Normal shutdown unlinks it; after an abrupt exit the
  existing local-socket binding code removes a stale, non-listening socket.
- During drain the old generation continues serving the private hook channel,
  local RPCs and exit reports. It stops accepting SSH handoffs, so new handoffs
  remain in the activated SSH socket's kernel queue until the new generation
  starts. Authorized-key lookup remains available on the shared local socket.
  Already accepted handoffs may finish admission as in 0157.
- The entire drain retains its 30-second bound, including RPC shutdown. A push
  exceeding it may still be interrupted; the bound and recovery policy do not
  change. The existing five-second helper reply bound also remains.

## Consequences

Session hooks cannot wait behind the restart of their own supervisor. An SSH push
also works with a non-default `run_dir` once handed off to that daemon; sshd's
initial helpers still use the default addresses as specified by 0093.

The signed receive-pack unit test uses the private channel while a simulated
activated listener remains unaccepted, including after admission closes. A separate
hook test checks that a missing session channel fails promptly without fallback.
Daemon tests check its permissions and cleanup. `nixos-server-lifecycle` releases a held
signed push during restart, requires it to finish without `drain-timeout`, and
checks that a second signed push queues and succeeds after restart. Both activated
socket inodes and the server's zero-refused-readyz check remain covered. Background
push status is recorded on failure too, and waits are bounded instead of hiding a
failed push behind the VM driver's default timeout.
