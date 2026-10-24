# ADR 0021 — Repository addressing and URLs

Status: Accepted — 2026-10-11 (revised 2026-10-24: ref resolution, validator, local creation and import accepted)

## Context

Flake inputs, clones and mirrors depend on stable URLs. No organizations: a single owner level.

## Decision

- A repository is identified by `<owner>/<name>`; `owner` is a user. Names: `[a-z0-9][a-z0-9._-]{0,62}`, not ending in `.git`, case-insensitive in resolution, stored in lowercase.
- **One shared validator** in `kajiba-core` for owner and repository names, ref names (`git check-ref-format` rules) and paths (no `..`, no empty segment, no NUL), used by every route, SSH command and RPC.
- HTTP routes (one port, 0005):
  - UI: `/<owner>/<name>` and sub-routes (`/tree/<ref>/<path>`, `/commits/<ref>`, `/runs/<id>`).
  - Smart HTTP: `/<owner>/<name>.git/info/refs`, `/<owner>/<name>.git/git-upload-pack`.
  - Raw: `/raw/<owner>/<name>/<ref>/<path>`.
  - API: `/kajiba.v1.*` prefix (gRPC, grpc-web), `/healthz`, `/readyz`, `/auth/webauthn/*`.
- **Ref resolution in `<ref>/<path>`**: the longest leading run of segments that names an existing branch, then tag, wins (`feature/x/README.md` is branch `feature/x` when it exists); otherwise the first segment must be a full commit id. The same rule serves `/raw/`, `/tree/` and `/commits/`.
- SSH: `ssh://kajiba@<host>:<port>/<owner>/<name>.git` or `kajiba@<host>:<owner>/<name>.git` on port 22 (the `.git` is optional in resolution).
- Flake inputs: `git+https://<host>/<owner>/<name>.git?ref=<branch>`; `shallow=1` possible.
- **Renaming**: the old `<owner>/<name>` is kept in the DB as a redirect (HTTP 301 for the UI, transparent resolution for git and SSH) as long as it isn't reused. On disk nothing moves (0012).
- **Creation before API authentication**: until J4, repositories are created only by host-local commands over the local socket (0003): `kajiba-server admin repo create <owner>/<name>` and `kajiba-server admin import <owner>/<name> <source>`, where `<source>` is a path to a bare repository, a bundle, or a URL to clone from (all refs and tags, history intact). From J4, `RepoService.Create` is available to authenticated users.

## Consequences

- URLs are known before the first line of code: modules, docs and tests use them as is.
- Adding organizations later amounts to allowing an `owner` that isn't a user: no URL change.
- A deployment moving from another forge changes each input URL once; the GitHub mirror stays a fallback with the same `narHash` (0015).

## Rejected alternatives

- `/repos/` or `/git/` prefix: longer URLs without benefit; conflicts with reserved routes are avoided by a list of forbidden names (`raw`, `kajiba.v1`, `healthz`, `readyz`, `auth`, `metrics`, …).
- An explicit `branch/`, `tag/`, `commit/` segment in every URL: unambiguous but longer, and unlike the URLs users expect from other forges.
