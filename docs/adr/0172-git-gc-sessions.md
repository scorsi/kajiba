# ADR 0172 — Git gc maintenance sessions

Status: Accepted — 2026-11-01. Amends 0022, 0052, 0053, 0070, 0157.

## Decision

- `kajiba-git gc` calls the peer-checked daemon socket. Add local
  `GitService.Gc`, with an empty request and an exit-status response. It accepts
  no command, path or repository supplied by the caller. The daemon enumerates
  canonical `<public-id>.git` directories under its configured `repos/`;
  malformed entries fail closed and quarantines are never visited.
- `Spawn` remains reserved and unimplemented; `Gc` is the maintenance entry
  point, with no caller-supplied session, service or repository (amends 0070).
- For each repository, sequentially, the daemon opens a system-principal
  Maintenance session through the server's existing per-repository gate.
  Registered archived repositories allow maintenance only; other archived
  sessions remain refused. Unknown repositories, pending imports/pushes, live
  writers and maintenance admission refusals stop the command before a child
  starts for that repository. Operators retry a busy command; there is no wait
  for a build or push to finish and no automatic retry loop.
- Run only `git gc --auto` as the daemon identity, with the existing isolated Git
  environment and group-read umask. Force `gc.autoDetach=false` so no background
  writer outlives the session; disable hooks and automatic maintenance. Preserve
  the repository's shared-object permissions. No aggressive/prune option exists.
- A detached RPC supervisor owns admission, child wait and exit recording even
  if the client disconnects. Retain exit facts for the existing SessionExited
  retry loop, and release the server gate only after child exit. Use existing
  maintenance session logs and metrics. A nonzero child status stops iteration
  and is returned to the CLI; admission/transport errors print the retry reason
  in structured stderr and exit unsuccessfully. Daemon shutdown refuses new
  maintenance registrations and retains its existing bounded drain/cgroup kill.

## Consequences

The server remains read-only on Git storage. Dumps drain admitted gc sessions
and refuse new ones. No timer, config option or dependency is added here.
