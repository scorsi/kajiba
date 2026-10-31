# ADR 0138 — Runs of a ref

Status: Accepted — 2026-10-31. Amends 0131.

## Context

A repository's Pipelines and a run page's "runs on this ref" show the runs of one branch. `ListRuns` lists a repository's runs, optionally by state (0131), so the web application could only filter by ref after fetching pages that mix every ref, and a busy branch would be hidden by the others.

## Decision

- **`ListRunsRequest.ref`.** When set, only runs whose `ref` equals it exactly are listed (a full ref such as `refs/heads/main`); empty means all refs. It combines with `states`, pagination and the newest-first order. A manual or worktree run has no ref and matches no non-empty value. An unknown or malformed ref is an empty page, not an error.
- **Usual duration in the browser.** The "usual" duration shown against a run is the median duration of the ref's recent succeeded runs, computed in the browser from the page of runs it already reads; the server computes no statistics.

## Consequences

- Additive proto change (`string ref = 5`); same scope and read role as before.
- The browser has no forge-wide run list: pages that need runs across repositories read each readable repository's most recent runs.

## Rejected alternatives

- A ref prefix or pattern: a branch is identified exactly, and patterns invite escaping and index questions.
- A server-side median: it would need a second method and a definition of "recent" the browser can choose freely.
