# ADR 0187 — Restore temporary trust boundary

Status: Accepted — 2026-11-01. Amends 0186.

## Decision

Pass an explicit trusted root alongside the temporary directory through restore
installation and SQLite scratch cleanup. Canonicalize both paths, refuse temporary
directories outside that root, and check ownership and permissions on every
directory below it. The trusted root and its ancestors are trusted by the caller.
The temporary path itself must still be a real directory, not a symlink or file.

Production passes the process temporary directory and `/` as the trusted root,
retaining the host protection rule. Restore tests pass their private 0700 fixture
root and its parent as the trusted root, so the fixture root is checked without
relying on ownership of sandbox ancestors. Negative path tests keep unsafe
ancestors below the boundary; synthetic UID tests retain foreign-owner coverage.

## Consequences

Linux Nix user namespaces can map sandbox ancestors to an overflow UID without
preventing tests from exercising the production restore installation path. No
conditional test bypass, public configuration option, dependency or protocol
change is introduced.
