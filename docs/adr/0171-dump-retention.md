# ADR 0171 — Daily dump and completed-dump retention

Status: Accepted — 2026-11-01. Amends 0016, 0019, 0022, 0169.

## Decision

- The NixOS module enables `kajiba-dump.timer`, `OnCalendar=daily` and
  `Persistent=true`. Its oneshot `kajiba-dump.service` runs
  `kajiba-server admin dump` as `kajiba-server:kajiba-server`, after and requiring
  the server. Reuse the module's hardening, with no writable state paths: the
  online server owns copying and rotation. The command needs only its admin socket.
  Discard its stdout directory result; retain structured stderr in the journal.
- Every successful online dump rotates `dump.dir` while still holding maintenance.
  Only real directories with the canonical `dump-<Unix milliseconds>-<ULID>` name,
  matching manifest creation time and a fully verified payload are candidates.
  Unrelated, incomplete, invalid and symlink entries are untouched. Order by
  manifest creation time, then name, never filesystem modification time.
- Keep the `dump.keep` newest completed dumps (default 7), always protecting the
  dump just published. Zero keeps one, so a successful command never returns an
  already removed backup. If the clock moves backwards, the new dump takes one
  slot and the other slots keep the newest older dumps.
- Rotate only after verified publication. Delete oldest first and stop on the
  first deletion error, logging `dump-retention-failed` at warning level. A
  rotation failure leaves the verified new dump usable and the command successful;
  extra old dumps may remain for the next successful pass. It does not change the
  success metric's meaning or remove the protected set. A failed creation never
  initiates rotation.

## Consequences

No new dependency or configuration option is needed. Full verification avoids
mistaking arbitrary or incomplete directories for owned backups. External-copy
coordination belongs to the later backup handoff task; readers must not assume
old dumps remain available during rotation.
