# ADR 0022 — Operations and observability

Status: Accepted — 2026-10-11, revised 2026-10-24 (log schema, metrics endpoint, validators: Accepted; event and metric lists: accepted)

## Context

kajiba's observability must interoperate with the open products of its ecosystem: nisshi fixes the log schema v1 (nisshi `docs/log-schema.md`) and how services write it (nisshi ADR 0009: one JSON object per line on stderr, no journald client); masu fixes the metric convention (masu ADR 0005: loopback listener, `<service>_<what>_<unit>`, closed label sets, at most 500 series). The node agent that collects journals and scrapes metrics (hikyaku) is configured by the deployment, not by kajiba. Precedent for a Rust service: kekkai ADR 0019.

## Decision

- **Logs**: every line `kajiba-server` and `kajiba-runner` write on stderr, in every subcommand, is one nisshi schema v1 object: `v` (1), `ts` (UTC, RFC 3339, exactly 9 fractional digits, `Z`), `level` (`debug`, `info`, `warn`, `error`; `trace` written as `debug`), `event` (kebab-case), optional `msg`, `err` only at `warn` and `error`, then the event's fields. Never a reserved field (`node`, `service`, `stream`, `seq`, `source`, `recv_ts`, `_*`). **Never a token, cookie, enrollment link, key material or `Authorization` header.** Written by a hand-written `tracing` layer (`tracing` stays the facade, per-target levels through `log.level` or `RUST_LOG`); a panic hook writes `fatal{err}`. No journald client: journald (or launchd's log file on macOS for the runner) is only the transport.
- **Correlation**: every HTTP/gRPC request gets a `request_id` (ULID), logged on each line it causes, carried into events (`correlation_id`, 0017) and job logs, and returned in errors (0020).
- **Events**: `start{version}`, `listening{addr}`, `metrics-listening{addr}`, `ready`, `stopping`, `fatal`, `config-invalid`, `migration-applied{version}`; `request{request_id,method,status,dur_us}` (method = gRPC method or route class, never a raw path with query); `auth-refused{request_id,reason}` (warn); `push{request_id,repo,ref,old,new,user}`, `push-refused{request_id,repo,ref,reason}` (warn); `run-state{run,state}`; `runner-connected{runner,version}`, `runner-refused{runner,reason}` (warn); `mirror-sync{mirror,result,attempts}`; `handler-failed{handler,event_id}` (error); `enrollment-issued{user}`; `dump-done{dir,dur_us}`. The server logs per request, so the module sets `LogRateLimitIntervalSec=30s` and `LogRateLimitBurst=30000` as `mkDefault` (nisshi 0009).
- **Metrics**: `GET /metrics` on `metrics.listen`, module option `services.kajiba-server.metrics.listen` (and `services.kajiba-runner.metrics.listen`), default `null` = no listener. Loopback only: refused otherwise by `config check` and asserted by the module. Hand-written Prometheus text 0.0.4, names per masu's convention. Closed labels only (no repository, user or path label):

  | Metric                                                                   | Labels                                                                                                       |
  | ------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------ |
  | `kajiba_build_info` (gauge 1)                                            | `version`                                                                                                    |
  | `kajiba_requests_total`                                                  | `kind` (`grpc`, `grpc_web`, `git_http`, `raw`, `ui`, `other`), `code` (gRPC code name or `2xx`…)             |
  | `kajiba_request_duration_seconds` (histogram 0.01, 0.05, 0.25, 1, 5, 30) | `kind`                                                                                                       |
  | `kajiba_pushes_total`                                                    | `result` (`accepted`, `refused`)                                                                             |
  | `kajiba_auth_refusals_total`                                             | `reason` (0020 reasons)                                                                                      |
  | `kajiba_runs_total`                                                      | `result` (`succeeded`, `failed`, `cancelled`)                                                                |
  | `kajiba_jobs_pending` (gauge)                                            | none                                                                                                         |
  | `kajiba_runners_connected` (gauge)                                       | none                                                                                                         |
  | `kajiba_mirror_syncs_total`                                              | `result` (`ok`, `error`)                                                                                     |
  | `kajiba_mirrors_failing` (gauge)                                         | none                                                                                                         |
  | `kajiba_handler_failures_total`                                          | `handler` (`ci`, `mirror`, `notifier`, `webhook`)                                                            |
  | `kajiba_handler_lag_events` (gauge)                                      | `handler` (`ci`, `mirror`, `notifier`, `webhook`)                                                            |
  | `kajiba_dump_last_success_timestamp_seconds`                             | none                                                                                                         |
  | `kajiba_runner_jobs_total` (runner)                                      | `result` (`succeeded`, `failed`, `cancelled`); runner-completed attempts only (`lost` is server-owned, 0008) |

- **Health**: `/healthz` (process alive); `/readyz` (migrations applied, DB reachable, directories present, and each subsystem **enabled in this build and configuration** is up — the local socket from J1, the sshd integration from J3; a subsystem not yet delivered is not checked). The module declares an `ExecStartPost` waiting on `/readyz`.
- systemd timers declared by the module: weekly `git gc --auto` per repository (`kajiba-gc.timer`), CI retention cleanup (daily, `kajiba-retention.timer`), mirror reconciliation (0011), daily dump (`kajiba-dump.timer`, 0016).
- systemd hardening: fixed user and group `kajiba` (sshd and file ownership need it, no `DynamicUser`), `ProtectSystem=strict`, `ReadWritePaths=` `stateDir` and `dump.dir`, `RuntimeDirectory=kajiba`, `NoNewPrivileges`, `PrivateTmp`, empty `CapabilityBoundingSet=`, `UMask=0027`.
- **Validators in tests**: every nixosTest that starts `kajiba-server` or `kajiba-runner` pipes `journalctl _SYSTEMD_UNIT=<unit> -o cat` into `nisshi validate --logs` after asserting the expected events exist, and runs `masu validate --metrics` on the live endpoint when it is enabled, checking that a counter moved. nisshi and masu are test-only flake inputs (0015). Unit tests pin the line format (timestamp, escaping, reserved names) and the exposition.

## Consequences

- Debugging is `journalctl -u kajiba-server -o cat | jq`; `journalctl` field filters do not see service fields.
- Alerts are the metrics store's job; kajiba sends nothing anywhere.
- Timers are tested in `nixosTest` (manual trigger through `systemctl start`).

## Rejected alternatives

- journald native protocol: a client to maintain, absent on macOS (nisshi 0009).
- OpenTelemetry: no trace collector; overhead without a consumer.
- `tracing-subscriber`'s JSON formatter as is: field names and timestamp format differ from the schema; mapping it costs as much as the hand-written layer.
- Rotating log files: redundant with journald.
