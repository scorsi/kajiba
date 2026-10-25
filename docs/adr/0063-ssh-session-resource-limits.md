# ADR 0063 — SSH session resource limits

Status: Accepted — 2026-10-25

## Context

J3 security review item 3 identifies SSH sessions without application concurrency
or lifetime bounds. The five-second descriptor hand-off timeout does not bound
accepted children, their descriptors or the sessions recorded by the server.
Systemd limits contain the whole daemon but cannot provide fair admission or
reclaim an individual stalled session. ADR 0040 already sets fixed concurrency,
idle and absolute limits for public HTTP reads.

## Decision

- **Amends 0053 — SSH admission and hand-off.** One daemon-wide semaphore admits
  at most **32 SSH connections**, shared by upload-pack and receive-pack. Acquire
  before spawning the hand-off task or calling `OpenSession`; do not queue excess
  connections. Close excess connections immediately, with a best-effort failure
  reply saying the session limit was reached. Incomplete hand-offs count against
  the same budget and retain their existing **five-second** timeout.
- **Amends 0053 — direct descriptor transport.** The helper still passes stdin,
  stdout and stderr with `SCM_RIGHTS`. The daemon now relays these through child
  pipes, using nonblocking I/O and one **16 KiB** buffer per stream. This replaces
  the claim that no Git bytes pass through the daemon: observing successful byte
  transfers is necessary to enforce inactivity bounds without trusting a client.
- Each spawned SSH child has a **60-second idle timeout** and a **one-hour
  absolute deadline**, matching 0040's values. A successful write to child stdin
  or to handed-off stdout/stderr refreshes idle time; reads alone and EOF do not.
  Absolute time starts at child supervision and never resets. Timers remain
  active while output is blocked, after input EOF, and after the child closes its
  streams. Idle includes time spent computing without transferring bytes.
- Timeout or relay failure closes the streams, kills the child's process group
  (including hooks and Git descendants), and reaps the child. Normal completion
  drains stdout/stderr under the same deadlines and kills any remaining group
  members. Keep the child unreaped until the group is killed to prevent signalling
  a recycled process id; `kill_on_drop` is fallback cleanup for the direct child,
  and 0052's systemd cgroup cleanup still applies on daemon shutdown.
- Only confirmed child exit invokes the existing `finished`/`SessionExited`
  path, with failure status for a timeout or I/O error. A wait failure retains
  the live session until generation recovery, as before. Exit notification retry
  and pending-push reconciliation remain governed by 0053; timeout is not proof
  that no refs were updated. The permit covers supervision and the bounded exit
  notification attempt and helper reply; replies have a **five-second** timeout.
- These limits are constants, not configuration. The budget is shared across
  users and repositories; import and maintenance keep their existing policies.
  No direct dependency or protocol message is added.

## Consequences

- Busy clients retry; silent operations exceeding a minute and operations lasting
  an hour must reconnect. A terminated push can have applied refs; clients must
  inspect repository state before retrying. Abruptly killed Git can leave lock or
  quarantine files, as with daemon termination; timeout does not delete repository
  data or bypass reconciliation.
- Stream buffers and in-flight SSH children are bounded. Kernel pipe/socket
  buffering and Git's internal allocations remain additional. This is admission
  control, not per-user fairness: one user can occupy all 32 slots.
- Relaying adds a bounded copy cost to SSH transfers. The helper remains separate
  from supervision: closing only the hand-off connection does not cancel a child;
  stream failure or a deadline still reclaims it.

## Rejected alternatives

- Only systemd task/memory limits or sshd connection settings: exhaustion affects
  the entire service and does not bound the lifetime of an admitted child.
- An absolute deadline without an idle bound: a silent peer can hold a slot for
  an hour; socket readiness alone does not demonstrate byte transfer.
- Per-user limits and new configuration keys: outside this fixed daemon-wide
  resource budget; introduce a separate fairness policy if needed.
