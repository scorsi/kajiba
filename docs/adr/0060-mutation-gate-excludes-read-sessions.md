# ADR 0060 — Mutation gate excludes read sessions

Status: Accepted — 2026-10-25

## Context

J3 review quality item 3 found that SSH `upload_pack` sessions occupied the
server's mutation gate. A clone could block a push, prevent its pre-receive
admission, or delay pending-row recovery indefinitely. ADR 0053's references to
live "sessions" in that gate are broader than ADR 0003's ref-mutation gate.

## Decision

- **Amends 0053 — session admission and recovery.** Only `receive_pack`,
  `import` and `maintenance` sessions occupy the server's mutation gate.
  `OpenSession` and snapshot rebuilding exclude `upload_pack` from that gate.
  References to live sessions blocking mutation admission or pending-row
  reconciliation in 0053 mean live mutating sessions.
- SSH `upload_pack` remains a session authorised by the server and tracked in
  the daemon's child table, session snapshots and lifecycle reporting. Its
  opening, continued execution or exit cannot block mutation admission or
  reconciliation, nor release a live writer's gate or import reservation.
- ADR 0003's durability rules remain unchanged: a writer's exit must be known,
  pending rows must reconcile before another mutation, and unexpected ref
  values retain the evidence and block mutations. Reader exit is not proof of
  writer exit. Generation checks and snapshot validation remain in force.

## Consequences

Clones can overlap pushes and recovery without holding the mutation gate.
The server needs no separate reader registry; the daemon already accounts for
read sessions. No RPC, schema or dependency change is required.

## Rejected alternatives

- Waiting for every repository session: a read-only user could indefinitely
  prevent writes by keeping a clone open.
- Dropping upload-pack sessions from the protocol: that would lose their
  existing authorisation and daemon lifecycle accounting.
