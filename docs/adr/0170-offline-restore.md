# ADR 0170 — Offline restore installation and rollback

Status: Accepted — 2026-11-01. Amends 0016, 0023, 0052, 0097, 0122, 0157, 0160.

## Decision

- `kajiba-server restore <dump-dir> --config <server.toml>` is an offline,
  root-only Linux operation. Configuration supplies both state directories and
  the Git service identity; configuration and secrets are not restored. The
  operator first stops both services and all four activating sockets (the runner
  socket may be absent). Restore checks their systemd state and pending jobs,
  refuses active/transitioning units, and holds both existing lifetime locks
  throughout installation. Concurrent daemon starts cannot acquire those locks.
  Restore does not stop, start, mask or reset units itself.
- Both destination directories must already exist, be disjoint real directories,
  have their service ownership and mode 0750, and contain nothing except their
  regular lifetime lock file. Provision missing directories before restoring.
  Neither destination may overlap the input dump. Never unlink the lock files.
- Validate with 0167's read-only verifier before copying. Stage each side privately
  on its destination filesystem, compare every staged directory/file and SHA-256
  against the verified manifest, and revalidate the input before installation.
  Set server content to kajiba-server ownership and Git content to the configured
  Git identity, directories 0750 and files 0640; lock files remain private.
- Remove excluded ephemeral-log indexes from the staged database, retaining
  historical runs/jobs/attempts/steps and purged byte lengths where supported by
  that schema. Do not migrate: normal startup alone migrates and reconciles, and
  fails active worktree runs whose excluded bundles are absent with bundle-missing.
- Switch the staged top-level trees by rename while both locks remain held. On
  any reported error, remove only trees this restore created, including trees
  already installed on the other side. Preserve input, pre-existing directories
  and locks; report rollback errors. A successful restore fsyncs both destinations.
  This is error rollback, not a cross-filesystem atomic transaction against power
  loss: after a crash an operator inspects/removes partial restored content while
  units remain stopped before retrying. Never automatically delete unknown state.

## Consequences

Restoring ownership requires root, but neither daemon gains a new privilege.
Operators keep sockets stopped until successful completion, then start normally;
failed/start-limited units may require `systemctl reset-failed` per 0160. No new
configuration option, dependency, migration, public RPC or backup tool is added.
