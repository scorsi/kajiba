# ADR 0061 — Session opening and cancellation

Status: Accepted — 2026-10-25

## Context

J3 review quality item 4 found that losing an `OpenSession` response after
server-side registration left mutation admission occupied. The SSH daemon
registered its session only after receiving the response, so it could not
report an exit for the server-generated identifier. Healthy heartbeats do not
rebuild the gate. ADR 0053 did not specify cancellation of incomplete opens.

## Decision

- **Amends 0053 — session opening.** The daemon supplies a canonical ULID
  `session_id` and its `generation` in every `OpenSession`. Imports reuse the
  server's durable reservation identifier from `Fetch.session`. Field 6 is
  renamed from `import_session_id` to `session_id`; generation is field 7.
  The server still authorises every request and refuses a stale generation.
- Repeating an open for the same live mutating session is idempotent: it
  returns the same session identity without acquiring admission again or
  consuming the import reservation again. Changing the kind, repository,
  principal, correlation or generation of that live identifier is refused.
  Authorisation and repository state are checked again on a retry.
- **Amends 0053 — cancellation.** If opening fails, the daemon starts no child
  and retains a `SessionExited` fact for the known identifier, even if it
  never received a session. Its existing connection loop retries that fact
  until acknowledged. Imports already retain their known session before
  opening; SSH now retains cancellations too.
- `SessionExited` also means that an attempted session will never start.
  Under the same writer lock as opening, the server remembers closed IDs and
  releases their admission and reconciles pending rows. A cancellation that
  arrives before a delayed open prevents that open; repeated cancellation is
  harmless. Closed IDs remain in memory until the daemon generation changes.
  A server restart discards them safely: old RPC handlers do not survive it,
  and the daemon never retries an open it has cancelled.
- ADR 0060's mutation-only gate and ADR 0003's proof-of-exit rules remain in
  force. Cancellation proves no child started; a timeout alone never proves
  that an already-started child exited.

## Consequences

A lost open response can no longer strand admission while both processes are
healthy. No new RPC, database migration or dependency is needed. The two
binaries and generated clients must be deployed together under 0053's exact
version contract. Closed-ID memory grows with completed or cancelled sessions
within a daemon generation; IDs cannot be aged out while delayed opens exist.

## Rejected alternatives

- Clearing admission on timeout: a process might still be writing.
- Sending cancellation without remembering it: a delayed open could acquire
  admission after cancellation was acknowledged.
- Rebuilding on every heartbeat: a missing daemon entry does not prove that
  an in-flight open will never start a child.
