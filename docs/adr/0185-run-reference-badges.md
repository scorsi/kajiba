# ADR 0185 — Consistent run reference badges

Status: Accepted — 2026-11-01. Amends 0151 and 0179.

## Context

The owner's UI feedback asks for equal visual treatment of worktree and branch
or tag runs, and for worktree run history to read as a section.

## Decision

CI run lists use the shared RunRef component for reference badges: `branch: main`
for `refs/heads/main`, `tag: v1.2` for `refs/tags/v1.2`, and the existing `worktree`
badge and identity for worktree runs. Branch and tag badges share the worktree
badge's style and spacing. Missing refs do not imply a branch or tag.

On a worktree run page, history links have no selected-run outline. Branch and
tag history retains its compact state strip and selected-run outline.

## Consequences

No API, dependency or configuration change. Worktree identity and branch health
rules remain as in 0151; compact repository worktree rows remain as in 0179.
