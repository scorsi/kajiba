# ADR 0151 — Worktree run identity

Status: Accepted — 2026-11-01. Amends 0097, 0131, 0138 and 0145.

## Context

A worktree run can build the same commit as a branch run, but checks local sources and may select only some checks. The UI must distinguish it without suggesting the branch passed or failed.

## Decision

- Add `WorktreeRunSpec.base_sha` (field 5) and `Run.base_sha` (field 14). The CLI reports HEAD before capture, including for a clean tree. The server accepts an empty value for older clients, otherwise requires 40 lowercase hexadecimal digits, and stores it in nullable `run.base_sha`. This is client-reported display metadata, never used for authorization, bundle validation, source selection or branch health. Existing runs have no recorded base.
- Keep using `Run.triggered_by` and `Run.checks`. Views show a worktree badge and “worktree by <user> on <short base commit>”, followed by selected checks if present. A missing historical base is labelled “unknown base”. No branch is inferred from the captured commit or the bundle prerequisites.
- Repository pipelines separate recent worktree runs from the selected branch’s runs. Run-page history separates worktree runs from other triggers. Worktree runs never determine branch health or a commit’s branch CI badge, even when the captured commit equals the default branch tip.
- `kajiba ci list` prints `worktree:<user>` in the ref column for worktree runs; the existing JSON representation gains `baseSha`.

## Consequences

No dependency is added. A nullable column preserves existing runs. Worktree history remains bounded by the recent repository run page, like other browser-composed activity.
