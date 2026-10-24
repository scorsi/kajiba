# ADR 0008 — Forge ↔ runner protocol: bidirectional gRPC, declarative enrollment

Status: Accepted — 2026-10-09 (enrollment confirmed on 2026-10-11; attempts and leases accepted 2026-10-24)

## Context

Runners may run next to the forge or on other machines, NixOS or nix-darwin, behind NAT, without any inbound port. Deployment is declarative (Nix modules, secrets as files): no manual enrollment step is acceptable. The forge may redeploy itself: the service restarts during jobs. No orchestrator is assumed.

## Decision

- `RunnerService` gRPC service (`kajiba.runner.v1`) separate from the public API, same port.
- **Declarative enrollment**: one shared secret per runner, generated once by the operator and provided as a file on both sides. Server module: `services.kajiba-server.runners.<name>.tokenFile`. Runner module (NixOS and nix-darwin): `services.kajiba-runner = { name, url, tokenFile, backends, tags, maxJobs }`. The runner authenticates with this secret; the server recognizes it by name and creates or updates the `runner` row on first connection. Revocation = removing the declaration or rotating the secret. Interactive enrollment (UI/CLI) may be added later for a machine outside Nix.
- The **runner initiates** the connection and announces its version (0023) and **capabilities**: `arch`, `os`, available execution backends, free-form tags, maximum concurrency.
- One **bidirectional stream** per runner: forge → runner (job assignment, cancellation, acknowledgments); runner → forge (heartbeat, acceptance, log chunks, step status, result, artifact metadata). Artifacts themselves are uploaded through a separate call.
- Matching: a job declares its requirements (`arch`, `os`, backend, tags); only compatible runners receive it.
- The runner is started by whatever you want (systemd, launchd); the protocol assumes no orchestrator.

### Attempts, leases, results

- A job is executed through **attempts** (`job_attempt`, numbered from 1). Every runner message about execution carries the attempt id; a message for an attempt that is not the job's current one is answered with "cancel" and otherwise ignored.
- **Leases are server-owned**: the server sets `lease_expires_at` from its own clock (default 30 s, renewed by each heartbeat, sent every 10 s); runner clocks are never compared.
- Job states: `pending → assigned → running → succeeded | failed | cancelled | lost`. Attempt states: `running → succeeded | failed | cancelled | lost`. Any other transition is refused by the domain layer.
- Lease expiry: the attempt becomes `lost`; a job declared replayable gets a new attempt while `attempts < max_attempts` (default 3), otherwise the job fails with reason `lease-expired`.
- **Terminal results are recorded once**: the first terminal state of an attempt wins, later ones are acknowledged and ignored. A cancellation recorded before the result wins over it.
- **Log chunks** carry a per-attempt sequence number; the server acknowledges the highest contiguous one; after reconnection the runner resends from there; duplicates are ignored.
- Reconnection: the runner resumes with the ids of its in-flight attempts; the forge answers continue (attempt still current and its lease not expired) or cancel.

## Consequences

- No inbound port on the runner side; a single port on the forge side.
- Status messages are idempotent (replay after reconnection).
- Redeploying the forge is a first-class test case (0015).
- The flake exports `nixosModules.runner` and `darwinModules.runner` in addition to the server module (0015).

## Rejected alternatives

- Enrollment token created in UI/CLI: a manual step, contrary to declarative deployment.
- Identity from an overlay network's peer identity (a mesh VPN's "who is this peer" API): zero secrets, but couples the forge to one network product and doesn't cover a runner on the same host over loopback.
- HTTP long-poll + JSON, WebSocket, home-grown TCP, forge → runner connections: all worse than outbound bidirectional gRPC.
