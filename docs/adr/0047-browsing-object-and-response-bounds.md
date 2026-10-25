# ADR 0047 — Browsing object and response bounds

Status: Accepted — 2026-10-25

## Context

ADR 0043 put the browsing RPCs under the read budget of 0040 and bounded their results by count. The third round of the J2 review found what those counts leave open: a unary gRPC response is encoded and delivered after its permit is released, so a 16 MiB `GetBlob` outlives the budget that admitted it; an object is loaded before its kind is checked, so a blob id given as a revision, as a ref target or as a forged cursor tip is read whole before being refused; trees and diffs are built in full before their count is checked; and commit objects have no size bound at all, so neither a `ListCommits` page nor a commit diff has a byte bound.

## Decision

- **Amends 0043 — `GetBlob` serves at most 1 MiB**, reason `BLOB_TOO_LARGE` above it. A unary response cannot stream under the 0040 deadlines, so its size is bounded instead; larger files are downloaded through `/raw/`, which keeps 0040's 16 MiB cap and streams under the 0040 bounds.
- **Object caps, checked on the header before the object is loaded.** A tree object larger than **4 MiB** is refused with `TREE_TOO_LARGE`; a commit object, or an annotated tag on the way to one, larger than **1 MiB** with the new reason `COMMIT_TOO_LARGE`. The caps apply to every object browsing reads whole: the revision, each tree on a path, each subtree a diff descends into, each commit of a page. The history walk itself reads commits to find their parents; the cap bounds what a page keeps, not that transient read.
- **Kind before load.** A revision, a ref target or a cursor tip is accepted as a commit only after its object header says so; a blob or tree there is `REVISION_NOT_FOUND` (HTTP 404 on `/raw/`) without being read.
- **Limits enforced during traversal.** A tree listing stops at the 0043 bound of 10 000 entries and a diff at 10 000 changes while they are being walked, not after. A diff is also refused past **4 MiB** of changed paths (`DIFF_TOO_LARGE`). A `ListCommits` page ends after the commit that takes its messages past **1 MiB**; the response then carries a page token, so a page can be shorter than requested and the next one may be empty, as at the end of history.
- With these bounds every browsing response is a few MiB at most, so what outlives a permit is small; the permit is still released when the response is built, as in 0043.
- `COMMIT_TOO_LARGE` extends the error table of 0020. Every limit here is a constant, not a configuration key. On `/raw/` a commit or tree object past its cap is HTTP 413 like an oversized blob.

## Consequences

- Files between 1 MiB and 16 MiB are no longer readable through the API; `/raw/` and Git serve them.
- A repository holding a tree object above 4 MiB or a commit object above 1 MiB is not fully browsable through the API; Git itself remains the way to read it.
- Clients paging history treat a short page with a token as a normal page.

## Rejected alternatives

- Carrying the permit through the encoded response: tonic encodes a unary response after the handler returns, and the transport's buffers live as long as the client's connection; 0043 already rejected that for `/raw/`.
- Streaming browsing responses: more API surface for results that a bound keeps small.
- Truncating long commit messages: the message is the content; a shorter page keeps it whole.
