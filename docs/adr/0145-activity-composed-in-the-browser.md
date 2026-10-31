# ADR 0145 — Activity composed in the browser

Status: Accepted — 2026-10-31.

## Context

The activity page (`/`) shows what happened across the forge: commits, releases, CI runs, and the states someone should act on. The server has no forge-wide event feed, and the journal (0017) is an operations log, not a user-facing history.

## Decision

- **Everything the viewer can read, nothing more.** The page is composed in the browser from the public RPCs: `ListRepos`, then per readable repository `ListCommits` on the default branch, `ListRefs` (tags are releases) and `ListRuns`. No server API is added; a repository the viewer cannot read contributes nothing, as for every other view (0013).
- **Runners and mirrors are for administrators (0139).** Their events and their "Needs attention" lines are loaded only when `GetViewer` says `admin`; for anyone else the page never calls those methods.
- **Needs attention** lists: a default branch whose latest finished run failed (queued and running runs do not mask it), a failing mirror, an offline runner. A manual run carries no ref (0010), so it counts for the default branch when it ran that branch's tip commit.
- **History** groups commits per day, author and repository; releases, failed runs, failing mirrors and offline runners are events of their own. The period is the Pulse period (30 or 90 days).

## Consequences

- The cost grows with the number of readable repositories (three calls each); a forge-wide feed RPC is a later decision, with its own ADR.
- Listing is bounded: the newest runs and commits per repository only.

## Rejected alternatives

- A server-side `ListEvents`: out of scope until the views have shown which queries they need.
- Showing runner and mirror state to every user: decided against in 0139.
