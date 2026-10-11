# ADR 0008 — Forge ↔ runner protocol: bidirectional gRPC, declarative enrollment

Status: Accepted — 2026-10-09 (enrollment confirmed on 2026-10-11)

## Context

Runners on ishizue (next to the forge) first, then jiban (nix-darwin) and future x86_64 machines behind Tailscale/NAT. All deployment is declarative in sekkeizu, secrets through sops: no manual enrollment step is acceptable. The forge redeploys itself: the service restarts during jobs. Nomad abandoned.

## Decision

- `RunnerService` gRPC service separate from the public API, same port.
- **Declarative enrollment**: one shared secret per runner, generated once by the user in sops and declared on both sides. Server module: `services.kajiba.runners.<name>.tokenFile`. Runner module (NixOS and nix-darwin): `services.kajiba-runner = { name, url, tokenFile, backends, tags, maxJobs }`. The runner authenticates with this secret; the server recognizes it by name and creates or updates the `runner` row on first connection. Revocation = removing the declaration or rotating the secret. Interactive enrollment (UI/CLI) may be added later for a machine outside Nix.
- The **runner initiates** the connection and announces its **capabilities**: `arch`, `os`, available execution backends, free-form tags, maximum concurrency.
- One **bidirectional stream** per runner: forge → runner (job assignment, cancellation); runner → forge (heartbeat, acceptance, log chunks, step status, result, artifact metadata). Artifacts themselves are uploaded through a separate call.
- **Lease** per job with heartbeat; expired lease = job marked lost then reassigned if the job is declared replayable, otherwise explicit failure.
- Reconnection: the runner resumes the stream with the identifiers of in-flight jobs; the forge reconciles (job still expected → continue, unknown → cancel on the runner side).
- Matching: a job declares its requirements (`arch`, `os`, backend, tags); only compatible runners receive it.
- The runner is started by whatever you want (systemd, launchd); the protocol assumes no orchestrator.

## Consequences

- No inbound port on the runner side; a single port on the forge side.
- Status messages must be idempotent (replay after reconnection).
- Redeploying the forge is a first-class test case (0015).
- The flake exports `nixosModules.runner` and `darwinModules.runner` in addition to the server module (0015).

## Rejected alternatives

- Enrollment token created in UI/CLI: a manual step, contrary to declarative deployment.
- Identity through Tailscale (`whois`): zero secrets, but couples the forge to the tailnet and doesn't cover a runner on the same host over loopback.
- HTTP long-poll + JSON, WebSocket, home-grown TCP, forge → runner: see initial discussion; all worse than outbound bidirectional gRPC.
