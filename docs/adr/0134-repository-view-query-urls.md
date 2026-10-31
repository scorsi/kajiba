# ADR 0134 — The repository view is one route with its state in the query

Status: Accepted — 2026-10-31. Amends 0021.

## Context

0021 lists UI sub-routes under a repository (`/tree/<ref>/<path>`, `/commits/<ref>`), which makes a path a part of the URL path and forces every view to split `<ref>/<path>` before it can render. The second design of the interface (owner decision, 2026-10-31) shows a repository as one page of foldable bands (README, Files, History, Pipelines) whose state, a selected file, a ref, a point in time, which bands are open, must be linkable. The server also serves the application shell only for paths without a file extension (0094), so a path-style link to `…/tree/main/README.md` is answered 404 whatever the interface does.

## Decision

- **Amends 0021 — UI routes.** The repository view is the single route `/<owner>/<name>`. Its state lives in query parameters: `ref` (a branch or tag short name), `path` (a file or directory), `at` (a commit id: time travel, with a way back to the tip), and the fold state of each band. The shell does not hold this state elsewhere, so every view of a repository is a link.
- `/<owner>/<name>/tree/<ref>/<path>` and `/<owner>/<name>/commits/<ref>` stay accepted: the browser replaces them with the query form, resolving `<ref>/<path>` by 0021's rule (the longest leading run of segments naming a branch or tag, otherwise a commit id), `commits` opening History. They only work where the server already answers with the application shell, which is the extension-less case.
- The rest of 0021 is unchanged: repository identity, names, renames (a previous name redirects to the current one), HTTP routes of other kinds, `/raw/`.

## Consequences

- Pages linking to a file use `?path=`; the raw bytes stay at `/raw/<owner>/<name>/<ref>/<path>`.
- No server change: only the browser reads the query.
