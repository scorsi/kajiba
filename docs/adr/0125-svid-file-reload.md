# ADR 0125 — SVID files reloaded before each handshake

Status: Accepted — 2026-10-31. Amends 0019, 0022.

## Context

The forge and each runner hold an X.509-SVID (0124) that lives about a day and is renewed in place by the deployment's CA, which writes `svid.pem`, `key.pem` and `bundle.pem` atomically (staged, synced, renamed, the key last) and signals no service. ADR 0019 reloads configuration by restarting the service; restarting the forge or a runner twice a day for a certificate would cut every stream, and a process that never reloads would fail its peers once its SVID expires.

## Decision

- **Amends 0019 — the identity files are reloaded in-process.** Before each TLS handshake, the forge's runner listener and the runner compare the device, inode and modification time of their three files with those of the material in use. When one differs, they load the SVID, the key and the bundle under a lock; a pair that does not load or does not match, an SVID without a valid ID in the trust domain, or an empty bundle (caught between two renames) keeps the previous material, and the next handshake tries again. Everything else in the configuration still needs a restart.
- One loader, in `kajiba-core`, backs the certificate the side presents and the bundle it verifies its peer against, so both change together. Open connections keep their handshake: a renewal never cuts a stream (0124 ends a stream when the peer's SVID expires).
- **Amends 0022 — log lines.** `svid-reloaded{not_after}` (info) when new material is in use; `svid-reload-failed{err}` (error) once per change of the files that does not load.

## Consequences

- A renewal is picked up by the next connection, without a signal or a restart; the CA needs no knowledge of the forge or the runners.
- Three `stat` calls per handshake, negligible next to the handshake.
- A process whose files stay broken keeps its last good SVID and logs the failure once; its peers refuse it when that SVID expires.

## Rejected alternatives

- Reload on `SIGHUP`: the CA signals no service, and a missed signal leaves an expired SVID.
- A timer polling the files: a second period to tune for what a handshake already triggers.
- A file watcher (inotify, kqueue): a dependency and platform code for the same result.
