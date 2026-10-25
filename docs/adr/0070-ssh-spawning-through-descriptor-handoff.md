# ADR 0070 — SSH spawning through the descriptor handoff

Status: Accepted — 2026-10-25

## Context

ADR 0053 describes the raw `ssh.sock` descriptor handoff and also lists
`GitService.Spawn` as the SSH spawning operation. J3 review quality note 12
identifies that SSH children actually start in the handoff handler; the RPC
always returns `UNIMPLEMENTED` and has no caller.

## Decision

- **Amends 0053 — `Spawn`.** SSH spawning uses only the raw handoff already
  specified there: `serve-ssh` sends the parsed command, user and descriptors
  to `ssh.sock`; the daemon obtains `GateService.OpenSession` admission and
  starts the child with those descriptors and the configured hooks path.
- `GitService.Spawn` remains a reserved, unimplemented RPC in J3, returning
  `UNIMPLEMENTED` / `NOT_IMPLEMENTED`. It is not an SSH entry point. Keeping
  this declaration avoids a protocol/generated-client change for a
  documentation correction. Maintenance (`git gc --auto`) remains J8 work;
  its implementation must settle the RPC's use or removal then.
- Session accounting, admission and exit reporting remain as specified in 0053. No child is started by this RPC.

## Consequences

- J3 documents one working SSH spawning path without adding maintenance work.
- A service test pins the reserved RPC's refusal. No wire contract changes.

## Rejected alternatives

- Routing SSH through `Spawn`: protobuf requests do not carry the descriptors
  already transferred by the raw handoff.
- Removing the RPC and generated clients in this documentation correction:
  unnecessary API churn for an unused entry point.
