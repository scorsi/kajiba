# ADR 0160 — Bound socket-activated startup failures

Status: Accepted — 2026-11-01. Amends 0157.

## Context

A pending connection can reactivate a failed socket-activated daemon even with
`Restart=no`. In particular, the server's readiness probe can leave a connection
queued when configuration validation exits. Disabling automatic restart alone
therefore does not leave an invalid configuration visibly failed.

## Decision

- Both `kajiba-server.service` and `kajiba-git.service` allow five starts in
  five minutes (`StartLimitIntervalSec=300`, `StartLimitBurst=5`). The budget
  covers automatic restarts, socket activation and manual starts together.
  It is shared across every socket activating the same service, including the
  server's optional runner listener and Git's local and SSH listeners.
- When another start exceeds that budget, systemd leaves the service failed
  with `Result=start-limit-hit`. For these `Accept=no` sockets, systemd
  propagates the service's start-limit failure to its socket units as
  `Result=service-start-limit-hit`, closes their listeners and stops activation.
  No separate per-socket trigger limit or stop-on-every-failure dependency is
  needed. Failed sockets stay failed after the service's rate interval expires.
- After correcting the configuration, an operator can use `systemctl
reset-failed` on the service and its sockets, then start the service; its
  existing `Requires` dependencies start its sockets again.
- Ordinary restarts within the budget retain their listeners and queued
  connections as specified by 0157. The lifecycle test resets the accumulated
  budget before its three consecutive valid restarts; listeners stay open.
- The server readiness probe monitors the main PID while its HTTP request is
  in flight, cancels the request when that process exits and fails within one
  100 ms polling interval, subject to scheduling. A live process still needs a
  successful `/readyz` response before the unit becomes active. Permission
  denied while the main process drops privileges does not count as exit.

## Consequences

Invalid configuration produces at most five startup attempts per failure burst,
then remains visibly failed rather than consuming CPU and journals indefinitely.
Each invalid attempt still emits exactly one `config-invalid` line. Repeated
manual restarts also consume the budget; an operator may need `reset-failed`.

The module evaluation check pins both budgets. `nixos-server-config` exercises
real startup failures for both daemons, checks all their socket failure states,
counts five invocations with one configuration error each, and checks server
recovery after repairing its secret. Its first invalid server start must fail
within five seconds, and both crash loops must stop within ninety seconds.
The five-minute budget also contains five attempts at the previously observed
6.5 seconds per cycle, so that delay cannot evade the start limit.
`nixos-server-lifecycle` retains its zero-refused-connections assertion across
valid restarts.
