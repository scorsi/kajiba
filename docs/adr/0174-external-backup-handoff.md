# ADR 0174 — External backup handoff and rotation coordination

Status: Accepted — 2026-11-01. Amends 0016, 0169, 0171.

## Decision

- `dump.dir` is the deployment-agnostic integration point. An external pre-backup
  hook runs `kajiba-server admin dump`, waits for success and copies only the
  returned completed directory. Configuration, secrets and the external tool
  remain deployment-owned; no hook option or backup-tool dependency is added.
- The stable regular file `dump.dir/.copy.lock` coordinates copying with rotation
  using an exclusive advisory flock. The server creates it mode 0600 when needed.
  A hook running as `kajiba-server` acquires it **before** requesting the dump and
  holds it until copying finishes. Never unlink or replace the lock file, even
  after a crash; closing the descriptor releases the lock automatically.
- Rotation opens the same file without following a symlink and tries the lock
  without waiting. While another holder copies, dumps can still be published,
  but the entire pruning pass is skipped. The next successful dump after release
  retries retention. Other lock/open errors retain all dumps and use the existing
  `dump-retention-failed` warning. Lock ownership never extends maintenance to an
  external copy, and the hook must not wait for rotation to acquire its lock.
- The manifest and atomic completed-directory publication remain unchanged.
  `.partial-*` directories and `.copy.lock` are not backup payloads. An external
  tool must finish reading under the lock or consume its own private completed
  copy; merely releasing the hook's lock before asynchronously reading `dump.dir`
  does not protect that read. Copy failure must fail the hook, never report a
  successful backup. Hooks serialize on this lock and must still retry a dump
  refused with MAINTENANCE.

## Consequences

No dependency, configuration option, protocol field or dump-format change.
A stuck copy can delay pruning and consume disk space, but cannot hold the forge
in maintenance. Other tools deleting dumps must honor the same advisory lock.
