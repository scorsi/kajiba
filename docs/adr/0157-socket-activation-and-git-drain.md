# ADR 0157 — Socket activation and bounded Git session drain

Status: Accepted — 2026-11-01. Amends 0015, 0022, 0052, 0053, 0054, 0140, 0141.

## Context

Closing a daemon's listening socket during a restart leaves a connection-refusal
window. Git's detached session supervisors also outlive its accept loop, while
systemd previously sent SIGTERM to their children at the start of a stop.

## Decision

- The NixOS module owns the server's HTTP and optional runner mTLS listening
  sockets in `kajiba-server.socket` and `kajiba-server-runner.socket`, using
  the configured addresses and descriptor names `http` and `runner`. Both
  activate `kajiba-server.service`; it requires and starts after its sockets.
  The TLS handshake and peer verification remain in the server.
- `kajiba-git.socket` owns `/run/kajiba-git/local.sock` (`local`, mode 0660),
  and `kajiba-git-ssh.socket` owns `/run/kajiba-git/ssh.sock` (`ssh`, mode 0600).
  Both belong to kajiba-git:kajiba-git, activate `kajiba-git.service`, and are
  required and ordered before it. Tmpfiles creates the parent with mode 0750;
  the service preserves its runtime directory, including across a full stop,
  so the socket units keep their paths. Peer UID checks remain mandatory.
- Both daemons consume systemd's `LISTEN_PID`, `LISTEN_FDS` and
  `LISTEN_FDNAMES`: only the current PID inherits descriptors starting at 3,
  each marked close-on-exec. Metadata is cleared before spawning children.
  Named descriptors may arrive in any order. Without names, their order is
  HTTP then runner, or local then SSH. Incorrect counts, duplicate or unknown
  names, non-listening descriptors and mismatched addresses fail startup.
  Without activation (including a different PID), daemons bind normally.
  Dropping an inherited Unix listener never unlinks its path.
- A service restart leaves its socket units running. The kernel queues new
  connections until the new process accepts them, within the kernel backlog
  and client timeouts. Changing a listening address necessarily replaces its
  socket; that change does not promise continuity on the old address.
- Amends 0140: the server retains its prompt stream termination and 5 s drain.
  The Git daemon stops accepting SSH handoffs and refuses new registered
  imports. Already accepted SSH handoffs may complete admission and run.
  Accepted SSH supervisors are awaited; active
  sessions and pending exit reports drain while hook RPCs and the server
  connection loop remain available. The shared local RPC listener remains
  available for hooks until sessions finish, then gracefully shuts down.
- Git's entire drain, including local RPC shutdown, is bounded to **30 s**,
  with no configuration option. On expiry it logs `drain-timeout` and exits 0,
  even if a blocking import worker remains. Startup reconciliation retains
  its existing interrupted-session recovery semantics. The Git unit uses
  `KillMode=mixed` so only the daemon receives the initial SIGTERM, with
  `TimeoutStopSec=35s` as a final bound on remaining children. Amends 0052
  and 0053: children get the drain before termination; systemd still empties
  the cgroup before a new generation starts. The separate
  sshd keeps its established sessions.
- Amends 0141: both services explicitly set `restartIfChanged=true` and
  `reloadIfChanged=false`. Server declarations still use `reloadTriggers`
  and SIGHUP; packages, UI and restart keys still restart. Git has only
  restart triggers. Socket units depend on listener configuration, not
  packages or declarations, so ordinary deployments retain the listeners.
- No protocol, configuration key or dependency is added. Shared activation
  code uses the existing libc dependency and standard library in kajiba-core.

## Consequences

Restarts queue new connections instead of refusing them. Existing server streams
still resume; Git operations that finish within the drain survive a Git restart.
Operations longer than the bound may be interrupted. Socket activation does not
remove backlog limits, client deadlines, or the need for stream resumption.

Unit tests cover activation metadata, inherited path ownership and Git draining.
The module evaluation check pins unit wiring. `nixos-server-lifecycle` hammers
readyz during server restarts and restarts Git during an admitted signed push,
checking success and preserved socket inodes.
