# ADR 0156 — Jobs outlive the Linux runner process

Status: Accepted — 2026-11-01. Amends 0008, 0009, 0013 and 0106.

## Context

A NixOS deployment restarts the runner and kills its service cgroup. Independent
process groups do not survive this. Z1 of the approved zero-interruption plan
requires the same attempt to complete after a runner restart, including its log.

## Decision

- On Linux, each attempt runs in its own transient system service,
  `kajiba-job-<canonical attempt ULID>.service`, created with `systemd-run --collect`
  and `Type=exec`. It has no dependency on the runner service and lives outside
  its cgroup. The existing `Executor` trait and Nix backend execute the complete
  job (including selected checks) inside that service. Darwin retains the child
  process backend; the default `systemd` Cargo feature and the Linux cfg select
  system units. Restart survival on Darwin is deferred until a launchd equivalent exists. The unprivileged fake-Nix
  e2e harness disables default features to keep its child-process fixture on Linux;
  production packages keep the feature enabled.
- The job uses the runner user/group, fixed HOME and PATH, the same untrusted
  Nix-daemon access, and the runner's filesystem, namespace, device, syscall and
  capability restrictions. It cannot bind sockets. Cancellation stops the whole
  unit cgroup, with a 10-second stop timeout. There are no credentials in its
  arguments or environment. The Nix sandbox remains the backend's boundary (0009).
- The NixOS module supplies a socket-activated, root-owned broker. Its Unix socket
  `/run/kajiba-job/control` is mode 0600 for the runner user. Requests contain only
  `start|stop` and a canonical attempt ULID. A fixed script supplies every unit
  property and both executable entry points; callers cannot supply commands,
  paths, users, environment or properties. Stop is idempotent for collected units.
  No polkit authority, sudo privilege or system-bus manage-units permission is
  granted to the runner. Prefix-only polkit permission would allow dangerous
  `StartTransientUnit` properties, including running arbitrary code as root.
- Each private `state_dir/jobs/<attempt>/state.json` holds the attempt, unit name,
  log and checkout paths, source SHA/system/checks, step, acknowledged sequence
  and byte offset, and the end offsets of unacknowledged chunks (at most the
  existing 16-chunk window). State replacement is atomic and synced. Chunk
  boundaries are persisted before transmission; acknowledgments are persisted
  before releasing their window space. Persistence failure terminates the runner
  so it cannot send bytes without a durable sequence mapping.
- The job opens `output.log` as the runner user and appends the Nix backend's
  merged stdout/stderr. Systemd does not open a runner-controlled log path as
  root. A fixed `ExecStopPost` records systemd's `SERVICE_RESULT`, `EXIT_CODE` and
  `EXIT_STATUS` atomically as `result.json` before the unit is collected. The
  backend's structural failing-check name is saved separately.
- Before its first hello, the runner loads these records and their indexed
  chunks. Existing units and completed jobs with a terminal marker are listed
  as in-flight, even if configured concurrency has since decreased. The existing
  ContinueAttempt/LogAck exchange reconciles them. Indexed chunks are replayed
  with identical sequences and byte boundaries, even when the server accepted a
  chunk whose acknowledgment was lost before the restart. Tailing resumes after
  the last indexed byte. The same step status and terminal result are resent
  until ResultAck; only then are the record, log and checkout removed. Cleanup
  first renames the record directory out of the adoption set; incomplete creations and cleanup
  remnants are discarded at startup.
- A server cancellation stops the unit before removing its files. A unit absent
  at startup without a terminal marker is omitted from hello and its files are
  removed: the server marks the attempt lost and replays it under 0008. An absent
  unit without a marker discovered while following fails the attempt. A failed
  broker start has an ambiguous outcome, so the runner exits and re-adopts on
  restart rather than reporting failure while a unit might still be executing.
- Scratch cleanup preserves checkouts with durable records. Restarts before
  unit creation may still lose/replay an attempt. Host reboot, lease expiry while
  the runner is down, state-directory loss, and deploying incompatible attempt
  record formats are not zero-interruption guarantees. Logs use local disk while
  the runner is down; operators must provision space for their jobs.

## Consequences

No public protocol or database schema changes. Reconnection still uses
server-owned leases and acknowledgments. NixOS installs the broker with the runner;
Linux execution requires it and systemd. The recovery test proves same-attempt
success, independent cgroups, and byte-identical logs after a service restart.

## Rejected alternatives

- Child process groups: still killed with the runner's service cgroup.
- Name-prefix polkit manage-units permission: does not constrain transient-unit
  properties and is a privilege escalation.
- Only saving the acknowledged byte offset: loses chunk boundaries when an ack
  races a crash and can skip or duplicate bytes on resumption.
