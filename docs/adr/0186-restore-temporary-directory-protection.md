# ADR 0186 — Restore temporary directory protection

Status: Accepted — 2026-11-01. Amends 0170.

## Decision

SQLite log cleanup uses a temporary directory outside the service-owned state
trees. The temporary root must be a real directory, not a symlink. Its canonical
path and every ancestor must be owned by root or the process's effective UID.
Group/world-writable directories are refused except root-owned sticky directories
such as `/tmp`. An owned private directory with mode 0700 needs no sticky bit.

Restore installation tests pass their own private 0700 temporary root under
canonical `/tmp`, rather than relying on the build sandbox's `TMPDIR` or changing
the process environment. Production continues to use the process temporary root.

## Consequences

The ancestor checks remain required: a private child cannot protect a pathname
under a directory an untrusted user can replace. No public option, dependency,
protocol or state format changes.
