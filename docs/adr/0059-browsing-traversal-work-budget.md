# ADR 0059 — Browsing traversal work budget

Status: Accepted — 2026-10-25

## Context

J3 review security item 8 found that the object and response bounds of 0047
and the history walk of 0051 do not bound graph expansion. A small history
page can queue many parents; a diff can traverse many directories and unchanged
entries while returning no files. Path lookup can scan large trees repeatedly.
Dropping an async handler does not stop its blocking worker.

## Decision

- **Amends 0047 and 0051 — bound traversal independently of results.** Each
  GitStore tree, blob path lookup, log or diff operation admits at most
  **25 000 commit/tree object reads**, **64 MiB of their uncompressed bytes**,
  and **100 000 tree entries or history parent edges**. Repeated object reads
  count again, including commits decoded for queue ordering and cursor skips.
  Individual object caps and response limits remain in force.
- Trees are checked and their entries charged before scanning them for a path
  or handing them to the diff engine. Every subtree occurrence counts, including
  unchanged entries in changed trees and directory-only changes. The diff engine
  may reread admitted subtrees once; its additional reads are therefore bounded
  by the same admitted objects and bytes. Identical subtrees it skips need no
  traversal. No repository-wide prewalk is performed.
- **History frontier:** at most **1024 pending parents**, checked before loading
  or inserting the next unseen parent. The object-read budget also bounds the
  visited set and history depth. Commit-time ordering, equal-time insertion
  ordering, cursor semantics, and stopping before the final page commit's
  parents are read remain unchanged.
- **Tree paths:** at most **128 components and 4096 bytes**, checked before
  lookup and before diff descent. These limits cover intermediate directory
  paths as well as returned file paths.
- **Cooperative deadline and cancellation:** each traversal has a **10-second**
  elapsed-work budget. Checks precede object reads, run during entry and parent
  iteration and diff callbacks, and run before successful completion. Dropping
  a browsing RPC handler sets a cancellation flag observed by its worker. The
  worker retains its read permit until it actually stops; successful responses
  retain the delivery-permit behavior of 0050.
- Exceeding these budgets returns `RESOURCE_EXHAUSTED` with reason
  `TRAVERSAL_TOO_LARGE`, extending 0020. Cancellation stops through the same
  refusal path, with no response needed by the dropped handler. The shared
  tree-path checks also apply to `/raw/`, where excess maps to HTTP 413.
  These are constants, with no dependency, configuration or protocol field added.

## Consequences

- Some repositories whose responses fit the earlier limits are refused because
  producing them takes too much work. Git transport still serves their content.
- This is cooperative cancellation, not interruption of a filesystem syscall
  or an individual gix decode. A check can run only after that operation returns.
  Ref enumeration and revision resolution are outside these traversal deadlines;
  0056 continues to govern revision input and candidate lookups.
- Tests cover exact budget boundaries, cancellation, expired deadlines, broad
  ancestry including skipped commits, deep empty directories, and repeated
  subtrees with mostly unchanged entries.

## Rejected alternatives

- Count returned files or commits alone: directories and queued parents consume
  work even when they contribute no response entries.
- Replace gix's diff algorithm: charging subtrees before its existing descent
  preserves its ordering and type-change behavior with a smaller change.
- Rely on dropping a blocking task: it keeps running and holding its permit.
