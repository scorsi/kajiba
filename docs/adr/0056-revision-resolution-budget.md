# ADR 0056 — Revision resolution budget

Status: Accepted — 2026-10-25

## Context

Security review item 1 found that anonymous revision resolution copied every
successively shorter slash-delimited prefix and searched the entire ref listing
for each one. A slash-heavy revision caused quadratic work in a blocking task
that continued after the request timed out. The response and object limits of
0043 and 0047 did not bound this input or the number of candidate lookups.

## Decision

- **Amends 0043 and 0047 — resolution input is bounded.** Browsing RPC revisions
  and the decoded `<ref>/<path>` portion of `/raw/` accept at most **4096 UTF-8
  bytes** and **128 slash-separated components**. Inputs exactly at either bound
  are accepted. These are constants, not configuration keys.
- The shared resolver checks these limits before opening the repository. Each
  browsing RPC and `/raw/` also checks them before repository lookup, permit
  acquisition or scheduling blocking work. Excess is `RESOURCE_EXHAUSTED` with
  reason `REVISION_TOO_LARGE` for RPCs, extending 0020, and HTTP 413 for `/raw/`.
- **Candidate work budget.** Resolution makes at most **256 candidate ref
  lookups**, 128 in each namespace, and one full commit-id lookup. It reuses and
  truncates one name buffer per namespace instead of copying every prefix, and
  binary-searches the existing sorted ref listing instead of repeatedly scanning
  it. Only exact names match. The longest branch, then longest tag, then full
  commit-id precedence of 0021 is unchanged.

## Consequences

- Oversized revisions and very deep raw paths are refused; Git transport still
  serves the repository. Ordinary revisions retain their resolution semantics.
- Request-controlled candidate work remains bounded after handler cancellation.
  Ref enumeration and sorting still depend on repository size; candidate search
  performs at most 256 binary searches with names bounded by the input cap. This
  is an operation-count budget, not a wall-clock deadline for filesystem access
  or tag peeling; existing object caps and read permits still apply.
- No dependency, configuration or protocol field is added.

## Rejected alternatives

- Relying on the response timeout: cancelling a handler does not stop its running
  blocking task.
- A hash table of all refs: another allocation when the existing sorted listing
  supports bounded binary search.
- Direct filesystem ref lookup: path-length limits can reject candidates that
  include a long file path before the resolver reaches their shorter branch.
