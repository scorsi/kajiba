# ADR 0055 — Repository modes from the git children's umask

Status: Accepted — 2026-10-25

## Context

ADR 0052 makes every repository group-readable and never group-writable, and enforces it with `core.sharedRepository=0640` (`git init --shared=0640`). With an octal `core.sharedRepository`, Git forces the setgid bit on every directory it creates or adjusts. Linux refuses that `chmod` when the process is not a member of the directory's group, which is the case in the Nix build sandbox and for any directory a test creates outside `/var/lib/kajiba-git`: `git init` dies with `Could not make .../refs writable by group`. macOS ignores the bit, so the failure showed only on Linux.

## Decision

- Amends 0052: `kajiba-git` no longer sets `core.sharedRepository`. Every Git child that `kajiba-gitstore` starts runs with umask 0027, whatever the parent's umask. Directories come out 0750, files 0640, and Git's immutable objects and packs 0440 (their 0444 creation mode minus the umask), so the modes 0052 requires hold without a Git setting.
- 0052's unit `UMask=0027` stays; the per-child umask makes the modes independent of the caller, in particular in tests.
- No setgid directories: the group of a repository is the group of the state directory it is created in, owned by `kajiba-git` (0052).

## Consequences

- Tests assert the modes of a fresh repository after `init`, `fetch`, `update-ref`, `pack-refs` and `set_head` instead of the config key.
- A future writer other than Git (hooks, 0052's receive-pack path) must also run with umask 0027.
