# ADR 0011 — Push mirroring to GitHub

Status: Accepted — 2026-10-09

## Context

One direction only: forge → GitHub. The mirror must work no matter what. Private branches wanted later (absent from GitHub). No contribution must enter through GitHub.

## Decision

- Push with **explicit refspecs** `+refs/heads/*:refs/heads/*` and `+refs/tags/*:refs/tags/*` (not `--mirror`): GitHub is an exact copy of branches and tags, deletions and force-pushes included; `refs/contrib/*` and future private refs never leave (exclusion by prefix or DB marker).
- Triggered by the `post-receive` event → **job queue in the DB, one coalesced job per repo** (a push during an in-progress mirror re-flags it as "to redo"), **capped exponential backoff, unlimited retries**. Plus a **periodic reconciliation** (timer) that compares local and remote refs.
- Per-mirror status (last success, last error, attempts) visible in the UI and the CLI from v0; external notification later (0017).
- v0 authentication: GitHub **fine-grained PAT** in sops (one token, scoped to the list of repos), HTTPS push. Evolution: **GitHub App** (installation tokens renewed automatically, same App for the OAuth of the issues phase and for auto-closing PRs opened on the mirrors).
- On the GitHub side, in v0: repo created by hand; issues, wiki, projects, discussions disabled; description and website field point to the forge; no file injected into the history. Pull requests cannot be disabled on GitHub: ignored in v0. Automation of creation and settings through the API later.

## Consequences

- The model contains `mirror` (repo, remote, credential, status) from v0.
- PAT expiration to watch (existing reminder) until the GitHub App.

## Rejected alternatives

- `--mirror`: would push isolated and private refs.
- SSH deploy key per repo: a distinct key per repo (GitHub forbids reuse), heavy management without automation.
- Cron alone: latency and no guarantee after a push.
