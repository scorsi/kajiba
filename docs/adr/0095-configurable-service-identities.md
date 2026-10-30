# ADR 0095 — Configurable service identities

Status: Proposed — 2026-10-30

## Context

ADRs 0052 and 0054 fix the system users `kajiba-server` and `kajiba-git`, and both daemons resolve them by name for their peer checks: the server admits only `kajiba-git` on the gate socket and gives it to group `kajiba-git`; the git side admits `kajiba-server` and `kajiba-git` on `local.sock` and only `kajiba-git` on `ssh.sock`. The e2e harness (0015, 0093) runs both daemons unprivileged as one ordinary user, on hosts where neither system user exists: both daemons stop at startup.

## Decision

- **Amends 0052 and 0054 — identities in the configuration.**
  - The server's file gains `git.user` (default `kajiba-git`): the only peer of the gate socket, which belongs to that user's primary group.
  - The git side's file gains `server_user` (default `kajiba-server`) and `user` (default `kajiba-git`): the peers of `local.sock`, and `user` alone on `ssh.sock`.
  - An unknown user still stops the daemon at startup; there is never a fallback to a broader peer policy.
- The NixOS module keeps the defaults and does not expose the options; the gate socket's group was the group named `kajiba-git`, the primary group of user `kajiba-git` in the module, so deployments see no change.

## Consequences

- A test run sets every identity to the current user: the peer checks still run, they admit that user.
- A configuration error can admit another local user; the configuration file is root-owned in a deployment, like the rest of the peer policy's inputs.

## Rejected alternatives

- User namespaces with a substituted `/etc/passwd` for the harness: unavailable on hosts that restrict unprivileged namespaces, and impossible on macOS.
- Skipping peer checks when the identities are missing: a missing user in production would silently open the sockets.
