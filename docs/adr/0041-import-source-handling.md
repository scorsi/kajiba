# ADR 0041 — Import source handling

Status: Accepted — 2026-10-25

## Context

ADR 0026 makes `admin import` the only way to fill a declared, empty repository from a path, a bundle or a URL. The J2 review found that an import could run forever, kept a source's credentials in the git command line, and accepted relative sources resolved against the server's working directory.

## Decision

- **Amends 0026 — accepted sources.** A source is an absolute local path (repository or bundle) or a URL. Relative paths and scp-like `user@host:path` sources are refused; an SSH source is written `ssh://user@host/path`.
- **Credentials.** Userinfo in an `http`/`https` source never reaches a git command line: it is removed from the URL and handed to git through the child's environment and a credential helper. Error messages and logs never contain userinfo, a query or a fragment of the source.
- **Isolation.** Every git child of the server starts from an empty environment plus an explicit minimum (`PATH`, no system or global git configuration, no terminal prompt). Import children also pin the allowed protocols (`http`, `https`, `ssh`, `git`; `file` only for local sources) and disable hooks.
- **Deadline.** Each git child of an import (`ls-remote`, then `fetch`) is killed after `IMPORT_TIMEOUT` = 1 hour; a stalled HTTP transfer also ends after 60 seconds below 1 byte/s. The admitted operation is owned by the server, not by the RPC: cancelling the client never leaves the repository's gate held, and the gate is always released and reconciled.

## Consequences

- One import takes at most about two hours before it fails cleanly. A very large first import over a slow link may need to be split or done from a local bundle.
- Killing a child does not reach a grandchild such as an `ssh` transport process; the gate is released regardless and the orphan ends with its connection.
