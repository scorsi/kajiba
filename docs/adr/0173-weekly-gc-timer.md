# ADR 0173 — Weekly gc timer

Status: Accepted — 2026-11-01. Amends 0022, 0052, 0172.

## Decision

- The NixOS module enables `kajiba-gc.timer`, `OnCalendar=weekly` and
  `Persistent=true`, targeting the oneshot `kajiba-gc.service`.
- The service requires and follows both daemons and invokes `kajiba-git gc`
  from the configured Git package as `kajiba-git:kajiba-git`. Reuse the shared
  hardening, restrict the client to Unix sockets, expose `/run/kajiba-git`
  read-only and hide the server state directory. Grant no writable state paths:
  only the daemon runs Git, through the server's maintenance admission gate.
- Allow the admitted command to finish without a service startup timeout.
  Discard stdout and journal structured stderr. A busy repository or dump
  maintenance fails the service normally; there is no automatic retry loop.
  The next scheduled invocation or an operator's manual start retries admission.

## Consequences

Weekly gc preserves the daemon's repository permissions and dump exclusion.
No new dependency, configuration option or direct repository writer is added.
