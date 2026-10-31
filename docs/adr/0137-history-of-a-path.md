# ADR 0137 — History of a path

Status: Accepted — 2026-10-31. Amends 0051, 0059.

## Context

The repository view shows the history of the file or directory being looked at, and a file's last change. `ListCommits` listed the commits of a revision only, so the web application could not ask for them, and comparing trees for every commit of a long history is work the traversal budget of 0059 does not bound by results.

## Decision

- **`ListCommitsRequest.path`.** When set, a page keeps only the commits whose change against their **first parent** touches that file or directory: the entry at the path (mode and object id) differs between the commit and its first parent, an absent entry counting as different from a present one (so a path deleted before the tip is found, and a root commit is compared with the empty tree). Merge commits are compared with their first parent only. Empty means no filter, and the listing is the one of 0051 unchanged. The path is validated as for browsing (0021).
- **Same walk, same order.** The walk of 0051 (commit-time order, header checks, frontier of 0059) is unchanged; the filter adds one comparison per commit walked past the cursor, reading the trees along the path in the commit and in its first parent. Each tree read is charged to the traversal of 0059 like any other.
- **Comparison budget (amends 0059).** A filtered call stops walking once, since its first comparison, it has read 4 000 objects, charged 40 000 entries or read 16 MiB; the hard bounds of 0059 stay above it and still refuse. The first comparison is always made, so every page advances.
- **Short pages.** A page may therefore hold fewer commits than requested, even none, and still carry a `next_page_token`. A page that reaches its size looks ahead for one more match and carries a token only if there is one, so the last page has none.
- **The cursor keeps the path and counts commits walked.** The token holds the first page's tip, the path and the number of commits walked (matching or not), so a resumed page starts exactly where the previous one stopped, without duplicate or gap, and a request's own `path` is ignored with a token, as its `revision` is. The bound of 0043 on the depth of a cursor applies to commits walked.
- **Web application.** A listing that needs a number of commits follows the tokens (at most 8 requests, since the server clamps a page at 200); a file's diff against its previous revision is the file at the revision before the latest commit that touched it, read with two `GetBlob` calls. Renames are not detected (a rename is a deletion and an addition).

## Consequences

- Additive proto change (`string path = 5`); no new permission, the method keeps its scope and read role.
- A history deep enough that skipping to a cursor and comparing exceed the 25 000 object reads of 0059 is refused as `TRAVERSAL_TOO_LARGE`; Git transport still serves the repository.
- A path that exists nowhere is walked to the end of history, in as many short pages as the budget requires.
- Tests cover a file, a directory, the root, a path absent at the tip but present earlier, a path through a file, merges compared with their first parent, resumption by the token without duplicates, the byte cut, and a budget cut on a long history of empty commits.

## Rejected alternatives

- Filtering in the browser from unfiltered pages: pages cut by size and by 0051 would hide older matches.
- Comparing with every parent of a merge: reports merges that only carry a side branch's change, which `git log -- path` simplifies away differently per option; the first parent is deterministic and matches what the commit view shows.
- A fixed number of commits compared per page: ignores the cost of deep paths and large trees.
