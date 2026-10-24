# ADR 0011 — Push mirroring to GitHub

Status: Accepted — 2026-10-09 (revised 2026-10-24: deletion handling accepted)

## Context

One direction only: forge → GitHub. The mirror must work no matter what. Private branches wanted later (absent from GitHub). No contribution must enter through GitHub.

## Decision

- Push with **explicit refspecs** `+refs/heads/*:refs/heads/*` and `+refs/tags/*:refs/tags/*` (not `--mirror`): GitHub is an exact copy of branches and tags, force-pushes included; `refs/contrib/*` and future private refs never leave (exclusion by prefix or DB marker).
- **Deletions**: the push uses `--prune` with those refspecs only, so a branch or tag absent locally is deleted on the remote, and nothing outside `refs/heads/*` and `refs/tags/*` is ever touched there. Private refs (later) are excluded from the local side by explicit refspecs, never by pruning.
- Triggered by the `push` event → **job queue in the DB, one coalesced job per mirror** (a push during an in-progress mirror re-flags it as "to redo"), **capped exponential backoff, unlimited retries**. Plus a **periodic reconciliation** (timer) that compares local and remote refs.
- Per-mirror status (last success, last error, attempts) visible in the UI and the CLI from v0; external notification later (0017).
- v0 authentication: GitHub **fine-grained PAT** read from a file (`mirror.credentials.<name>.token_file`, 0019), HTTPS push. Evolution: **GitHub App** (installation tokens renewed automatically, same App for the OAuth of the issues phase and for auto-closing PRs opened on the mirrors).
- On the GitHub side, in v0: repo created by hand; issues, wiki, projects, discussions disabled; description and website field point to the forge; no file injected into the history. Pull requests cannot be disabled on GitHub: ignored in v0. Automation of creation and settings through the API later.
- A mirror enabled on a remote that already has the same refs pushes nothing (the adoption case of an imported repository, 0021).

## Consequences

- The model contains `mirror` (repo, remote, credential, status) from v0.
- The PAT expires: the operator tracks its expiry until the GitHub App; an expired token shows as a mirror error.

## Rejected alternatives

- `--mirror`: would push isolated and private refs.
- SSH deploy key per repo: a distinct key per repo (GitHub forbids reuse), heavy management without automation.
- Cron alone: latency and no guarantee after a push.
