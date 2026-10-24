# ADR 0043 — Browsing and raw response limits

Status: Accepted — 2026-10-25

## Context

ADR 0040 bounds anonymous reads with one shared budget of 32 permits over `info/refs`, `git-upload-pack` and `/raw/`, and gives streaming upload-pack bodies an idle timeout and an absolute deadline. The second round of the J2 review found three gaps: the browsing RPCs (`GetTree`, `GetBlob`, `ListCommits`, `GetCommitDiff`) schedule blocking Git reads outside that budget and return unbounded trees and diffs; a `/raw/` body is one allocation with no response deadline, so a client that opens 32 large downloads and stops reading holds every permit for as long as its connections live; and the `ListCommits` cursor carries an offset that a caller can forge, making one request decode an entire history before discarding it.

## Decision

- **Amends 0040 — browsing RPCs join the budget.** The four browsing RPCs take a permit from the 0040 semaphore after validating the request and resolving the repository, before scheduling their blocking Git read, and hold it until the response is built; a cancelled handler leaves the permit with the running work. Exhaustion is `UNAVAILABLE` with `ErrorInfo.reason` `READS_BUSY`, the gRPC counterpart of 0040's HTTP 503. No work is queued.
- **Response bounds.** A tree of more than **10 000** entries is refused with `RESOURCE_EXHAUSTED`, reason `TREE_TOO_LARGE`; a commit diff of more than **10 000** changed files with reason `DIFF_TOO_LARGE`. `GetBlob` keeps 0040's 16 MiB cap, reason `BLOB_TOO_LARGE`. A result exactly at a bound is served.
- **`ListCommits` traversal.** The cursor still names the first page's tip and an offset, so pages stay stable under pushes and follow every parent of a merge, but skipped commits are walked without being decoded, and the offset is bounded at **10 000**: the server never issues a deeper cursor, so a deeper one is forged and refused with `RESOURCE_EXHAUSTED`, reason `HISTORY_TOO_DEEP`. Older history is listed from an older revision.
- **Amends 0040 — `/raw/` bodies stream.** A raw blob is delivered in bounded chunks under the upload-pack bounds of 0040: the 60-second idle timeout without bytes handed to the HTTP transport and the one-hour absolute deadline. The sending task owns the blob allocation and the permit and releases both when the body ends or when a bound fires; the HTTP transport's buffers hold copies, never the permit.
- The reasons above extend the error table of 0020. Every limit here is a constant, not a configuration key.

## Consequences

- A busy forge refuses browsing calls instead of queuing them; clients retry, as for smart HTTP.
- Very wide trees, commits touching more than 10 000 files, and history deeper than 10 000 commits from one revision are not reachable through the API in one request; Git itself remains the way to get everything.
- Each `ListCommits` page still walks from the tip; the offset bound caps that cost.

## Rejected alternatives

- Paginating trees and diffs: more API surface for cases that get a clear refusal and that Git serves better.
- A cursor on the last returned commit: cheaper per page, but a walk restarted from one commit loses the interleaving of the other parents of a merge that the tip-and-offset cursor keeps.
- Keeping the permit with the raw blob allocation until the HTTP transport drops it: the transport's lifetime is the client's to extend.
- Configurable limits: as in 0040, constants give one explicit policy.
