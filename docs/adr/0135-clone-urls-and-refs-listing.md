# ADR 0135 — The server advertises clone URLs and lists refs

Status: Accepted — 2026-10-31. Amends 0019, 0020, 0026.

## Context

The repository page offers "Clone" and a ref switcher. Building clone URLs from the browser's location is wrong behind a proxy or when the SSH endpoint differs from the web one, and browsing needs the branches and tags a repository holds, which no method returned.

## Decision

- **Clone URLs come from the server.** `Repo.clone_urls` carries `https` = `http.public_url` + `/<owner>/<name>.git` and `ssh` = `ssh://<login>@<host>:<port>/<owner>/<name>.git`, where `<host>` and `<port>` are `ssh.host` (default: the host of `http.public_url`) and `ssh.port` (0019), and `<login>` is the git side's system user (0054). An archived repository has none (no git transport, 0026).
- **`RepoService.ListRefs(repo)`** returns the refs under `refs/heads/` and `refs/tags/` (0013), sorted by name, each with the commit it resolves to; an annotated tag is peeled and its tag object id is kept in `tag_id`. Other namespaces (`refs/contrib/*`, ...) and tags naming no commit are not listed. Scope `repo:read`, same read role and public-read budget as the other browsing methods (0020, 0043), `REPO_NOT_FOUND` when the caller cannot read the repository.

## Consequences

- Additive proto change; the clone panel and the empty-repository push commands show what the server advertises.
- Amends 0019: `ssh.host`, `ssh.port` and `http.public_url` also feed `Repo.clone_urls`. Amends 0020 and 0026: `RepoService` gains `ListRefs`, still read-only.
