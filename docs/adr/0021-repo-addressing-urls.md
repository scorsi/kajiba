# ADR 0021 — Repository addressing and URLs

Status: Accepted — 2026-10-11

## Context

sekkeizu's flake inputs and the mirrors depend on stable URLs. No organizations: a single owner level.

## Decision

- A repository is identified by `<owner>/<name>`; `owner` is a user. Names: `[a-z0-9][a-z0-9._-]*`, case-insensitive in resolution, stored in lowercase.
- HTTP routes (one port, 0005):
  - UI: `/<owner>/<name>` and sub-routes (`/tree/<ref>/<path>`, `/commits/<ref>`, `/runs/<id>`).
  - Smart HTTP: `/<owner>/<name>.git/info/refs`, `/<owner>/<name>.git/git-upload-pack`.
  - Raw: `/raw/<owner>/<name>/<ref>/<path>`.
  - API: `/kajiba.v1.*` prefix (gRPC, grpc-web), `/healthz`, `/readyz`, `/auth/webauthn/*`.
- SSH: `kajiba@<host>:<owner>/<name>.git` (the `.git` is optional in resolution).
- Flake inputs: `git+https://<host>/<owner>/<name>.git?ref=<branch>`; `shallow=1` possible. sekkeizu's `scorsi-*` inputs change URL once, during the migration; `via-github.nu` remains the fallback (0015).
- **Renaming**: the old `<owner>/<name>` is kept in the DB as a redirect (HTTP 301 for the UI, transparent resolution for git and SSH) as long as it isn't reused.

## Consequences

- URLs are known before the first line of code: modules, docs and tests use them as is.
- Adding organizations later amounts to allowing an `owner` that isn't a user: no URL change.

## Rejected alternatives

- `/repos/` or `/git/` prefix: longer URLs without benefit; conflicts with reserved routes are avoided by a list of forbidden names (`raw`, `kajiba.v1`, `healthz`, `auth`, …).
