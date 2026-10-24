# ADR 0024 — Decisions taken during J1

Status: Accepted — 2026-10-24

## Context

J1 (server core: configuration, database, events, observability) is the first milestone with code. From J1 on, an accepted ADR is not edited (README): the points the accepted ADRs left implicit, or that implementation showed had to be fixed, are recorded here. Each item names the ADR it amends; everything else in that ADR stands. Each item describes what the J1 code does and is covered by its tests.

## Decision

- **Amends 0022 — events added to the list.**
  - `repo-orphaned{path,destination}` (warn): at startup, after migrations and while holding the state lock, a directory under `repos/` that matches no registered repository is moved to `tmp/orphans/` (a numeric suffix `.1`, `.2`, … avoids overwriting an earlier quarantine) instead of being deleted. Everything else under `tmp/` is cleared at startup; `tmp/orphans/` is kept across restarts.
  - `metrics-failed` (error): `/metrics` could not read the handler lag from the database; the scrape answers 503.
- **Amends 0022 — the `request` event.** `status` is the integer HTTP status of the response. gRPC and grpc-web requests add `grpc_code`, the gRPC code name (`OK`, `NOT_FOUND`, `INTERNAL`, …), taken from the headers or the trailers; plain HTTP requests have no `grpc_code`. A request dropped before any response has no `status`, and gRPC requests then log `grpc_code` `CANCELLED`. The metric `code` label (gRPC code name or HTTP status) is unchanged.
- **Amends 0022 — `fatal` means the process exits.** A failure to generate a request id is logged as a `request` event at `error` with HTTP status 500 and the request is refused; the process keeps serving. `fatal` is written only on a path where the process then exits (startup failure, invalid command line, failed host-local command, panic hook).
- **Amends 0021 — reserved names.** The forbidden owner and repository names are every name starting with `kajiba.` (every gRPC package of the product: `kajiba.v1`, `kajiba.runner.v1`, `kajiba.admin.v1`, …) or `grpc.` (gRPC reflection and other `grpc.*` services), plus `raw`, `healthz`, `readyz`, `auth` and `metrics`, compared after lowercase normalization, in the shared validator of `kajiba-core`.
- **Amends 0020 — error reasons.** The transport attaches an `ErrorInfo` with domain `kajiba` and `metadata.request_id` to every non-OK gRPC status. A non-`INTERNAL` error whose handler supplied no `ErrorInfo` gets an empty `reason`: only the handler knows the situation behind a code, so no reason is invented. `INTERNAL` keeps reason `INTERNAL`, the message `internal error`, and no other detail; the original message goes to the `request` log line only.
- **Amends 0005 and 0020 — reflection.** When `http.reflection` is enabled, only gRPC server reflection v1 (`grpc.reflection.v1.ServerReflection`) is served; v1alpha is not.
- **Amends 0013 and `docs/schema.md` — lowercase `user.name`.** Lowercase user names are enforced in the database by `BEFORE INSERT` and `BEFORE UPDATE OF name` triggers (migration 6) that abort with the same message as a CHECK. SQLite cannot add a CHECK to an existing table without rebuilding it, and the rebuild's `DROP TABLE` would cascade through the foreign keys, because migrations run in a transaction where foreign keys cannot be disabled.
- **Amends 0012 — state directory creation.** `stateDir` and its sub-directories (`repos`, `db`, `ci/logs`, `ci/artifacts`, `tmp`, and `tmp/orphans` when needed) are created with the process umask and no explicit mode; the module's `UMask=0027` gives 0750. Each newly created directory and each orphan move is followed by an `fsync` of the affected parent directories.
- **Amends 0014 — enrollment links.**
  - Format: `<public_url>/auth/enroll#<token>`, where the token is 64 lowercase hexadecimal characters (32 random bytes). The token is in the URL fragment, which browsers never send to servers or proxies, so it stays out of request and proxy logs. The database stores its SHA-256 only.
  - `admin enroll` for a disabled user fails with `FAILED_PRECONDITION` (reason `INVALID_STATE`); for an unknown user, with `NOT_FOUND` (reason `USER_NOT_FOUND`).
  - Issuing a link writes no event to the journal (0017); it is logged as `enrollment-issued{user}` only, never with the link or the token.
  - That issuing a new link invalidates the previous one is verified in tests through the database (`enrollment_token`), until an admin RPC lists outstanding links.
- **Amends 0015 and 0022 — the server module.**
  - The `ExecStartPost` readiness probe polls `/readyz` only while the main process is alive: a server that exits during startup fails the start job at once instead of waiting for `TimeoutStartSec`. The probe writes nothing to the journal.
  - The module has typed options only and no raw `settings` escape hatch: an unknown option is an evaluation error. Secrets are only absolute file paths (strings, never Nix paths copied to the store); token entries are SHA-256 hashes.
  - Scopes and durations are plain strings in the module; `kajiba-server config check`, run at build time on the generated file, is their single validator. The generated file is exposed at `/etc/kajiba/server.toml`.
- **Amends 0019 — configuration validation.** `config check` refuses an SSH key fingerprint declared by two users for the same usage (authentication or signing; a user listing its own key twice is accepted) and a token hash declared twice, naming both fields, instead of failing on the database's unique constraints at startup. Token hashes must be exactly 64 lowercase hexadecimal characters.
- **Amends 0002 — dependencies added during J1.** kajiba has no closed dependency list; the direct dependencies added in J1 are recorded here:

  | Crate                                                             | Used by               | Why                                                                                                          |
  | ----------------------------------------------------------------- | --------------------- | ------------------------------------------------------------------------------------------------------------ |
  | `ssh-key` (`alloc`, `ecdsa`)                                      | `kajiba-core`         | OpenSSH public key parsing and SHA256 fingerprints (owner-approved); its `sha2` re-export also hashes tokens |
  | `serde`, `toml`                                                   | `kajiba-core`         | configuration (0019)                                                                                         |
  | `sqlx` (`sqlite`, `runtime-tokio`, `migrate`, `macros`)           | server                | database and embedded migrations (0012)                                                                      |
  | `tracing`, `tracing-subscriber` (`registry`, `std`), `serde_json` | `kajiba-core`, server | the log layer of 0022                                                                                        |
  | `http`                                                            | `kajiba-core`         | `public_url` parsing (`Uri`) in configuration validation                                                     |
  | `http-body`, `hyper-util` (`tokio`)                               | server                | already transitive: gRPC trailers in the request body wrapper, Unix-socket client for host-local commands    |
  | `tonic-types`                                                     | server                | `google.rpc.Status` and `ErrorInfo` details (0020)                                                           |
  | `tonic-reflection` (`server`)                                     | server                | gRPC reflection v1 (0005)                                                                                    |

## Consequences

- The nisshi validator and the J1 tests know the two extra events and the `grpc_code` field; dashboards read `status` as an integer for every request.
- A stray repository directory is never lost: an operator inspects `tmp/orphans/` and removes or re-imports it.
- Clients cannot rely on a reason for errors whose handler sets none; each handler adds its reason as RPCs arrive (J2 onward).
- A future table change on `user` keeps the triggers or replaces them with a CHECK in a rebuilt table, with foreign keys handled explicitly.
- Each later milestone that changes an accepted ADR records its decisions the same way.

## Rejected alternatives

- Deleting unregistered repository directories at startup: irreversible on a misconfiguration.
- Inventing a reason (such as `UNKNOWN`) for errors without handler details: a client could branch on a value no handler chose.
- Rebuilding the `user` table to add a CHECK: cascades through foreign keys inside the migration transaction.
- Putting the enrollment token in the query string or the path: it would appear in proxy and request logs.
- A raw `settings` option in the module: unknown keys would only fail at build time, and secrets could be written inline.
