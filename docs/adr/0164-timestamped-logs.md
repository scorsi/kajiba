# ADR 0164 — Timestamped attempt logs

Status: Accepted — 2026-11-01. Amends 0008, 0121, 0142 and 0156.

## Context

The run console shows 0:00 on every line. Sequenced bytes alone cannot recover
production times after buffering, reconnecting or restarting the Linux runner.

## Decision

- Add `int64 produced_at_ms` to runner `LogChunk` (field 5) and public `LogBytes`
  (field 3): UTC epoch milliseconds, captured at the executor's merged pipe read,
  before channel backpressure. Zero means unknown, including old peers and logs.
  This is informational; leases and sequencing still use the server's clock and
  sequence numbers. Runner and server clocks should be synchronized for display.
- Migration 18 adds `log_chunk.produced_at_ms INTEGER NOT NULL DEFAULT 0`.
  Index `(attempt_id, offset)` for bounded chunk lookup.
  Store the timestamp with the first accepted chunk; duplicates do not replace
  it. Empty and discarded chunks do not change earlier timestamps. The truncation
  marker shares the crossing chunk's timestamp; the 64 MiB bound is unchanged.
- Stored and following `WatchRun` reads respect indexed chunk boundaries, also
  when starting inside a chunk. Pieces of a chunk share its timestamp. Byte
  offsets, acknowledgments and the CLI's raw output are unchanged.
- Linux job execution appends production boundaries to `output.times`: fixed
  16-byte records, little-endian end byte offset (`u64`) then UTC milliseconds
  (`i64`). Publish each complete record after its bytes. Tailing waits for a
  complete record, preserving production boundaries across runner restarts.
  Pending durable chunks retain timestamps alongside their end offsets; replay
  uses the identical timestamps. Old records without times remain readable as
  unknown. As in 0156, host reboot and local disk loss are outside the guarantee.
- `api.ts` passes attempt start and chunk times to the browser's log decoder.
  Each completed line uses the completing chunk's timestamp; an unfinished line
  uses the latest chunk contributing to it. Multiple lines in a chunk share the
  time, with no interpolation. Display elapsed m:ss since the server-recorded
  attempt start, using the console's existing rounding. Clamp negative elapsed
  times to zero; missing timestamps display 0:00. Repeated bytes cannot retime
  lines already received. A new attempt starts its own time base.

## Consequences

Old runners, servers and clients remain wire compatible. Historical logs retain
0:00; clock skew may affect informational line times. Linux output has one small
sidecar record per produced chunk, retained and cleaned up with its job record.
