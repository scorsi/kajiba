# ADR 0169 — Online admin dump command

Status: Accepted — 2026-11-01. Amends 0016, 0020, 0022, 0052, 0053.

## Decision

- `kajiba-server admin dump` requests `AdminService.Dump` over the existing
  peer-checked local admin socket. Its empty request selects no paths or options;
  the server uses the running configuration's `dump.dir`. The response's `dir`
  is the completed directory, printed on stdout by the command. There is no
  request deadline: admitted writes and Git completion must drain first.
- Close admission atomically; concurrent dump requests receive UNAVAILABLE with
  reason MAINTENANCE. Drain as in 0166, then create and publish the verified
  payload of 0167. Copying runs on a blocking worker owning the quiescent guard;
  losing the requesting connection does not release maintenance during copying.
  Reads continue and the server never writes the Git state directory.
- Only successful publication emits `dump-done{dir,dur_us}` (duration includes
  drain) and sets `kajiba_dump_last_success_timestamp_seconds` to the completion
  time in Unix seconds. The gauge starts at zero after process startup. Failures
  preserve the previous success value, release maintenance and return INTERNAL;
  details are logged on the server, not exposed in the response.
- Restore remains the offline `kajiba-server restore <dump-dir>` operation;
  its installation and failure handling belong to the later restore task.

## Consequences

The command returns only verified dumps. No public RPC, timer, backup-tool option
or external copy/rotation coordination protocol is introduced by this command.
