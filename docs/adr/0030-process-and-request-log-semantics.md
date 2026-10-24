# ADR 0030 — Process and request log semantics

Status: Accepted — 2026-10-24

## Context

J1 implemented the log layer, the metrics endpoint, the listeners and the module's readiness probe (0015, 0022). Implementation and the J1 security review showed points of their observable behaviour that ADR 0022 left implicit. As README says, an accepted ADR is not edited: each item below names the ADR point it amends; everything else in that ADR stands. Each item describes what the J1 code does and is covered by its tests.

## Decision

- **Amends 0022 — the `request` event.** `status` is the integer HTTP status of the response. gRPC and grpc-web requests add `grpc_code`, the gRPC code name (`OK`, `NOT_FOUND`, `INTERNAL`, …), taken from the headers or the trailers; plain HTTP requests have no `grpc_code`. A request dropped before any response has no `status`, and gRPC requests then log `grpc_code` `CANCELLED`. The metric `code` label (gRPC code name or HTTP status) is unchanged.
- **Amends 0022 — `fatal` means the process exits.** A failure to generate a request id is logged as a `request` event at `error` with HTTP status 500 and the request is refused; the process keeps serving. `fatal` is written only on a path where the process then exits (startup failure, invalid command line, failed host-local command, panic hook).
- **Amends 0022 — events added to the list.**
  - `metrics-failed` (error): `/metrics` could not read the handler lag from the database; the scrape answers 503.
  - `accept-failed{err}` (warn): accepting a connection on the public, metrics or local listener failed with an error other than a peer giving up (for example `EMFILE`). The listener backs off for one second before accepting again.
- **Amends 0015 and 0022 — the readiness probe.** The module's `ExecStartPost` readiness probe polls `/readyz` only while the main process is alive: a server that exits during startup fails the start job at once instead of waiting for `TimeoutStartSec`. The probe writes nothing to the journal.

## Consequences

- The nisshi validator and the J1 tests know the extra events and the `grpc_code` field; dashboards read `status` as an integer for every request.
- An alert on `fatal` always means a process exit.
- File descriptor exhaustion slows accepting down instead of spinning on the error.
