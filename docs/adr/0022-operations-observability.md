# ADR 0022 — Operations and observability

Status: Accepted — 2026-10-11

## Context

A single host, journald available, no Prometheus or trace collector yet.

## Decision

- **Structured** logs through `tracing`, journald output (no log files), per-target level adjustable through `RUST_LOG` or the config. Every gRPC request and every job carries a correlation identifier.
- Health: `/healthz` (process alive); `/readyz` (migrations applied, DB reachable, hook socket open, directories present). The NixOS module declares an `ExecStartPost` waiting on `/readyz`.
- systemd timers declared by the module: weekly `git gc` per repository (`--auto`), CI retention cleanup (daily), mirror reconciliation (0011), daily dump (0016).
- systemd hardening: no `DynamicUser` (fixed user for sshd), but `ProtectSystem=strict`, `ReadWritePaths=stateDir`, `NoNewPrivileges`, `PrivateTmp`, empty `CapabilityBoundingSet=`.
- Metrics: Prometheus `/metrics` added when a collector exists; internal counters are instrumented from the start through `tracing` (spans), to avoid re-instrumenting.

## Consequences

- Debugging is done with `journalctl -u kajiba`; no additional tool to deploy.
- Timers are tested in `nixosTest` (manual trigger through `systemctl start`).

## Rejected alternatives

- OpenTelemetry from the start: no collector, useless overhead.
- Rotating log files: redundant with journald.
