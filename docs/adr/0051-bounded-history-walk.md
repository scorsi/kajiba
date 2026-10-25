# ADR 0051 — Bounded history walk

Status: Accepted — 2026-10-25

## Context

ADR 0047 capped the commit objects browsing reads whole at 1 MiB, checked on the object header before the load, and left one read outside that check: the history walk reads each commit to find its parents and their times, so an ancestor was loaded before its header was looked at. The final J2 verification showed what that allowance leaves open: a small valid tip with an oversized ancestor makes `ListCommits` or `GetCommitDiff` allocate that ancestor, and the part of a walk a cursor passes over bypasses the page's own check entirely. In J2 only the operator's imports put content in a repository; with J3's pushes any repository writer can.

The same verification found the page budget of 0047 short: only message bytes counted, while author names, author emails and parent ids are copied into the response as well.

## Decision

- **Amends 0047 — the product walks history itself.** `ListCommits` and `GetCommitDiff` no longer use the library's revision walk. The product keeps a queue of commits ordered by commit time, newest first and in insertion order among equal times, and before reading any commit, the revision, an ancestor passed over by a cursor or one returned, it checks the object header: a non-commit is `REVISION_NOT_FOUND`, a commit object above the 0047 cap of 1 MiB is `COMMIT_TOO_LARGE`. The parents of the commit that completes a page are not read. 0047's sentence "the cap bounds what a page keeps, not that transient read" is withdrawn: every read of the walk is under the cap.
- **No pre-receive refusal for this bound.** Refusing oversized commit objects at push time would leave history imported before J3 unchecked, and Git itself keeps serving such a repository. The bound lives where the objects are read, so it covers every history whatever its origin; a repository holding such a commit stays, as 0047 says, not fully browsable through the API.
- **Amends 0047 — the page budget counts every field a page keeps.** A `ListCommits` page ends after the commit that takes the sum of its messages, author names, author emails and parent ids past 1 MiB, not its messages alone. Commits stay whole; the short-page and page-token semantics of 0047 are unchanged.

## Consequences

- An oversized ancestor within a page's look-ahead refuses the page with `COMMIT_TOO_LARGE`, as a direct request for it does; it is never allocated.
- A commit is decoded twice by the walk, once for its time when queued and once when built; the order of a page is the one 0043 relies on.
- Tests prove that an ancestor cut after its header is refused beyond the page and at a cursor boundary without being read, that the cap is exact, that merges keep commit-time order across pages, and that large author fields or many parents end a page with tiny messages.

## Rejected alternatives

- Refusing commit and tree objects above the 0047 caps at `pre-receive` or at import: protects new content only; history already hosted keeps the gap, and the walk still has to trust what it reads.
- Keeping the library's walk and checking headers after each yielded item: the library reads a commit's parents before yielding it, so no check can precede that read.
- Budgeting the encoded response size: the same bound, measured after the work is done; summing the fields a page keeps stops before the next over-budget commit is built.
