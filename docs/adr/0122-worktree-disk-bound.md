# ADR 0122 — Worktree runs: active limit and bundle lifetime

Status: Accepted — 2026-10-31. Amends 0097 and 0118.

## Context

ADR 0097 bounds the forge's disk use for worktree runs at "4 × 64 MiB per user" through two rules: at most 4 non-terminal worktree runs per user, and a bundle that lives as long as its run. The two do not give that bound. A finished run frees its active place at once while its bundle stays until `ci.worktree_retention`, so a user can accumulate bundles batch after batch; and the limit counted only committed runs, so uploads in progress could each store up to 64 MiB before being refused. The owner decided on 2026-10-31, also raising the limit, which a developer running checks for several systems reached quickly.

## Decision

- **Active limit** (amends 0097's table and 0118's order, same place and codes): at most **16** active worktree runs per user, a product constant. An upload in progress counts as active from its first message, once admitted, until it ends; the count is taken under the write lock that creates runs, and checked again there before the run is created. Beyond: `RESOURCE_EXHAUSTED`, `WORKTREE_RUNS_LIMIT`.
- **Bundle lifetime** (amends 0097's "lives exactly as long as its run"): a worktree run's bundle is deleted as soon as the run is terminal, by the forge's scheduling pass (at least every second) and by the retention pass. The run, its jobs, attempts and logs stay until `ci.worktree_retention`, as before. A bundle whose run row does not exist yet (its creation has not committed) is never touched.
- **Bound.** Stored bundles per user are therefore at most 16 × 64 MiB = 1 GiB, uploads included.

## Consequences

- Forge disk use for worktree bundles is at most 1 GiB per user, whatever the retention.
- `ci logs` and `ci watch` on a finished worktree run work until retention; the run cannot be replayed, as 0097 already said.
- A restore (0016) finds only active runs' bundles to miss; `bundle-missing` (0097) applies to them as before.

## Rejected alternatives

- A per-user byte budget over stored bundles: the same bound with more bookkeeping.
- Keeping finished runs' bundles until retention: no use once the run is terminal, and the cause of the unbounded growth.
