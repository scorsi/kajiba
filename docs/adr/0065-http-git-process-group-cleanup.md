# ADR 0065 — HTTP Git process group cleanup

Status: Accepted — 2026-10-25

## Context

J3 security review item 9 finds that HTTP cancellation and deadlines kill only
the immediate upload-pack child. Descendants can continue consuming resources
after the public-read permit is released. Imports already supervise process
groups using `libc` (0046).

## Decision

- **Amends 0040 — HTTP Git cleanup covers the process group.** Both advertisement
  and upload-pack children lead dedicated process groups. Cancellation, either
  deadline, I/O failure and normal completion kill remaining group members before
  reaping the child and releasing the read permit. Observe exit with
  `waitid(WNOWAIT)` so the unreaped child reserves the group id until cleanup.
  A drop guard kills the group if supervision is dropped; `kill_on_drop` remains
  fallback cleanup for the immediate child. Existing limits and responses remain.
- **Amends 0034 and 0046 — another direct consumer of `libc`.** `kajiba-server`
  directly uses the existing `libc` 0.2 dependency for `waitid` and group signalling.
  No dependency version is added or changed.

## Consequences

- HTTP read permits cover descendant cleanup as well as immediate-child reaping.
  As with import supervision, descendants that deliberately leave the process
  group are outside this guarantee; deployment cgroups remain the outer boundary.

## Rejected alternatives

- Killing only the immediate child: Git helpers can outlive it.
- Reaping before group cleanup: the process id may be reused before signalling.
