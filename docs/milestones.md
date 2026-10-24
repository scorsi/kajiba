# kajiba — milestone plan

Technical milestones, no calendar. Each milestone has a verifiable **deliverable**, explicit **acceptance criteria**, the **tests** that prove them, and the **delegable** part (tests, boilerplate, refactoring). A milestone is closed when its criteria pass on `aarch64-linux` in `nix flake check` and, from J4, in the `e2e` job (0015); a milestone never leaves `nix flake check` red in between. Checks that need a real deployment are not product criteria: they belong to the deployment repository.

ADR references in parentheses. nixosTest names are flake attributes `checks.<linux system>.<name>`.

---

## J0 — Skeleton: a green repository at the first commit (done)

**Deliverable**: the `kajiba` monorepo (0018) with a GitHub mirror, `nix flake check` green, no feature.

Content (historical): flake (flake-parts, crane, treefmt-nix), devShell, Cargo workspace with the six empty crates, empty `kajiba-cli` **Go** module (replaced by a Rust crate in J1, 0007), empty SvelteKit project, `proto/` with `kajiba/v1/health.proto` and `buf.yaml`, `nix/` with packages, `nixosModules.{server,runner}`, `darwinModules.runner` and a trivial `nixosTest` (`nixos-healthz`), `docs/adr/`, `LICENSE` (Apache-2.0), `README.md`, `CLAUDE.md`, `CONTRIBUTING.md`, treefmt.

**Acceptance criteria**

- `nix flake check` green on `aarch64-linux`; green on `aarch64-darwin` for the non-VM checks.
- `nix build .#kajiba-server .#kajiba-runner .#kajiba-cli .#kajiba-ui` produce artifacts; `kajiba-server --version` prints the workspace version.
- The "identical versions" check passes (0023).
- `buf lint` and `buf generate` run.
- The `nixosTest` starts `kajiba-server` through the module, `/healthz` answers 200.
- GitHub mirror in place, GitHub repo configured per 0011.

Known gaps, repaired in J1 tasks 1–4: the checks do not run workspace tests nor build the CLI and UI; `kajiba-proto` is not built by any check; generated code freshness is unchecked; package versions in `nix/packages/*.nix` are literals outside `versions-identical`; code comments name a specific deployment.

---

## J1 — Server core: config, database, events, observability

**Deliverable**: a server that starts from a Nix-generated configuration, reconciles declared identity, migrates its database, journals events durably, writes schema-valid logs, exposes metrics, and issues passkey enrollment links from the host.

Ordered tasks; each is closable in one sitting with its unit tests, in one or a few commits, and leaves `nix flake check` green.

1. **Check baseline repair** (0015, 0018): `checks` gain crane `cargoTest` and `cargoClippy` over the workspace and the builds of all four packages; server and runner depend on `kajiba-proto` so its `build.rs` runs; `svelte-check` on the UI. A broken shared crate fails a check.
2. **CLI skeleton: Go → Rust** (0007, 0018): `apps/cli` becomes the `kajiba-cli` crate (binary `kajiba`, clap, `--version`); delete the Go module, `go.work`, `apps/cli/internal/genproto`, the `go_package` option, `buildGoModule`, Go and the Go protoc plugins from the devShell, gofmt from treefmt, the Go `.gitignore` lines; `kajiba-proto` also builds the tonic client. Test: `kajiba --version` prints `VERSION`.
3. **Versions and generated code** (0020, 0023): `versions-identical` checks `VERSION`, `Cargo.toml` and `apps/ui/package.json`; Nix packages read `VERSION`; the devShell puts `apps/ui/node_modules/.bin` on `PATH` (`protoc-gen-es`); check `proto-generated` regenerates the TypeScript client and fails on a diff; check `buf-breaking` against the last tag (informational, skipped while no tag exists).
4. **Product wording in code** (0001): flake description, module comments and stubs no longer name a deployment.
5. **Command structure** (0002, 0019): `kajiba-server serve --config`, `config check [--files] <path>`, `admin …` placeholders, `--version`; exit codes. Tests: parsing, usage errors exit 2, unknown subcommand.
6. **Shared name validator** (0019, 0021): owner, repository, runner and credential names in `kajiba-core`, reused by configuration; lowercase normalization, length, `.git` suffix and reserved route names per 0021. Tests: valid names, case normalization, invalid characters, length boundaries, suffix and reserved names.
7. **Configuration types** (0019): serde structs with `deny_unknown_fields`, defaults, durations, validation rules of 0019 using the shared name validator; the server and runner examples of 0019 as test fixtures. Tests: examples load; unknown field, inline `token`, non-loopback `metrics.listen`, bad hash, unknown scope, bad name each fail with the field named.
8. **Log layer** (0022): hand-written `tracing` layer writing schema v1 lines, panic hook `fatal`, `start`, `config-invalid`. Tests pin `ts` (9 digits, `Z`), escaping, `err` only at warn/error, no reserved field.
9. **State directory layout and lock** (0012, 0023): `stateDir` layout and `kajiba.lock` single-instance lock before opening the DB. Tests: directories created, lock released on exit, second instance refused.
10. **SQLite and migration policy** (0012, 0023): connection policy, sqlx migrations, `migration-applied`, newer schema refusal and rollback on migration failure. Tests: fresh DB, reopen, connection pragmas, unknown newer migration refused, failing migration rolls back.
11. **Schema: identity** (docs/schema.md): `user`, `ssh_key`, `api_token`, `webauthn_credential`, `session`, `enrollment_token`. Tests: uniqueness, cascades, CHECKs, ULID `public_id`.
12. **Schema: repositories**: `repo`, `repo_redirect`, `repo_permission`, `mirror`. Tests: owner/name uniqueness, redirect pair uniqueness, RESTRICT on owner deletion.
13. **Post-schema cleanup** (0012, 0023): startup `tmp/` cleanup and orphan sweep of `repos/`, after the `repo` table exists; move orphan directories to `tmp/orphans/` and log them. Tests: temporary files removed, orphan moved, registered repository preserved.
14. **Schema: events** (0003, 0017, docs/schema.md): `event`, `handler_checkpoint`, `pending_push` with ordered per-ref old/new sha and one admitted push per repository. Tests: append and read back, pending admission uniqueness; no event update path exists in the repository layer.
15. **Schema: CI**: `runner`, `run`, `job`, `job_attempt`, `step`, `log_chunk`, `artifact`; `event` already exists for `run.event_id`. Tests: state CHECKs, `UNIQUE(run.event_id)`, cascades.
16. **User reconciliation and revocation** (0014): declared users and `is_admin` → DB at startup; removed user disabled with sessions and runtime PATs revoked, repositories kept; idempotent. Tests: add, change, remove, revocation scope, rerun with no change = no write.
17. **Key and token reconciliation** (0014): declared auth and signing keys and PAT hashes → DB at startup; removed key or declared token deleted; idempotent. Tests: add, change, remove, runtime items preserved, rerun with no change = no write.
18. **Transactional dispatch and checkpoints** (0017): append inside the caller's transaction, in-process wake-up, per-handler ordered dispatch, effect + checkpoint in one transaction, event-derived effect uniqueness. Tests with a test handler: rolled-back event is not dispatched; effect and checkpoint commit together; replay produces no duplicate effect.
19. **Handler retry and crash/replay verification** (0017, 0022): resume from checkpoints at startup, capped-backoff retry, `handler-failed`, failures counted by `kajiba_handler_failures_total{handler}`. Tests: event committed then process killed before dispatch → handled once after restart; failed handler transaction → effect rolled back, checkpoint unchanged, retried; replay from a checkpoint produces no duplicate effect.
20. **HTTP/gRPC transport, health and reflection** (0005, 0020, 0022): axum + tonic on `http.listen`, `/healthz`, `/readyz` (enabled subsystems only), reflection by `http.reflection`. Tests: HTTP and gRPC served on one port, not ready before migrations, reflection off by default and available when enabled.
21. **Request correlation, error details and shutdown** (0005, 0020, 0022): `request_id` per request in logs and `ErrorInfo`, `request` event, graceful shutdown on SIGTERM (`stopping`). Tests: error carries `request_id` matching its log; SIGTERM drains in-flight requests and exits cleanly.
22. **Metrics** (0022): `metrics.listen` loopback listener, `kajiba_build_info`, `kajiba_requests_total`, `kajiba_request_duration_seconds`, `kajiba_handler_lag_events`, `kajiba_handler_failures_total{handler}`. Tests pin the exposition format and closed handler labels; one failed handler attempt increments its failure counter once.
23. **Local socket and admin transport** (0003, 0014): `/run/kajiba/local.sock` (owner `kajiba`, 0600) serving `kajiba.admin.v1`; host-local client transport and socket readiness. Tests: root and `kajiba` can connect, unprivileged user refused, `/readyz` fails when the socket is unavailable.
24. **Enrollment lifecycle** (0003, 0014): `kajiba-server admin enroll <user>` prints a single-use URL (hashed, TTL, replaces the previous link) after revoking the user's passkeys, sessions and runtime PATs; `enrollment-issued` logged without the link. Tests: revocation scope (declared PAT kept), replacement, expiry, unknown or disabled user refused. Consuming the link is J4.
25. **NixOS options and TOML validation** (0015, 0019, 0022): `services.kajiba-server` options mirroring the configuration, file generated and checked at build time by `config check`, loopback metrics assertion. Check `module-eval` (all systems) evaluates a full configuration and rejects invalid options.
26. **NixOS service lifecycle, hardening and persistence** (0015, 0019, 0022): fixed user and group `kajiba` (replacing `DynamicUser`), `RuntimeDirectory`, hardening, `ExecStartPost` on `/readyz`, `LogRateLimit*` defaults, `persistedPaths` read-only option, `kajiba-server` on root's `PATH`. Check `module-eval` (all systems) asserts the generated unit, identity and persisted paths.
27. **Test inputs** (0015, 0022): nisshi and masu as test-only flake inputs; a shared nixosTest helper asserting `nisshi validate --logs` on a unit's journal and `masu validate --metrics` on an endpoint. Check: the four following nixosTests use the helper and pass; replace `nixos-healthz` with them.
28. **`nixos-server-lifecycle`** (0012, 0014, 0015, 0023): service ready (`/readyz` 200); a declared user and key present in the DB; reboot with persisted `stateDir` → rows intact; `stateDir` wiped → clean re-migration; a second `serve` on the same `stateDir` refused. Closing test: `nixos-server-lifecycle`.
29. **`nixos-server-config`** (0015, 0019, 0022): the module's file passes `config check --files`; a file with an inline secret or an unknown field is refused by `config check`; a missing `token_file` stops startup with `config-invalid`. Closing test: `nixos-server-config`.
30. **`nixos-server-observability`** (0015, 0022): `nisshi validate --logs` on the journal (with `start`, `ready`, `request` lines present, `request_id` on request lines); `masu validate --metrics` on the loopback endpoint, `kajiba_requests_total` moved; the endpoint is not reachable from another node. Closing test: `nixos-server-observability`.
31. **`nixos-server-enroll`** (0003, 0014, 0015, 0022): `kajiba-server admin enroll alice` as root prints a link; as an unprivileged user it fails on the socket; a second call invalidates the first link (checked through the admin API). Closing test: `nixos-server-enroll`.

**Acceptance criteria**

- The four nixosTests above pass; every check of tasks 1–27 passes on all systems where it runs.
- An event written then the process killed before dispatch is handled exactly once after restart (Rust integration test).
- No Go toolchain left in the repository or the flake.

**Delegable**: SQL migrations, fixtures, replay tests, module options, nixosTest scaffolding.

---

## J2 — Declared repositories, read-only git hosting

**Deliverable**: repositories declared in the configuration are reconciled at startup (created, renamed with redirects, archived when removed with their content kept in place, unarchived when re-declared), filled by `admin import`, clonable over smart HTTP and through `/raw/`, browsable through a read-only API; Nix can use them as inputs (0026).

Ordered tasks; each is closable in one sitting with its unit tests, in one or a few commits, and leaves `nix flake check` green.

1. **Repository configuration** (0019, 0026): `repos.<key>` with `owner`, `name`, `description`, `visibility`, `default_branch`, `ci_enabled`, `previous_names`, `permissions.<user>`, `mirrors[]` (`remote_url`, `credential`, `enabled`); `config check` rules of 0026 (declared owner and permission users, unique current names, previous names neither current nor repeated, declared credentials, unique remote per repository). Tests: a full example loads; each refusal names its field.
2. **Module options for repositories** (0015, 0026): `services.kajiba-server.repos` mirroring task 1, written to the generated file and checked at build time. Check `module-eval` evaluates declared repositories and rejects an unknown option.
3. **Schema: declared repositories** (docs/schema.md, 0026): migration adding `repo.config_key` (unique index) and `repo.archived_at` by `ADD COLUMN` (the existing `UNIQUE(owner_id, name)` stays and covers archived rows, so `repo` is not rebuilt), and making `pending_push.principal_id` nullable (`system`). Tests: key uniqueness, an archived row's name refused for another row, a `system` pending row accepted, migration history updated.
4. **Ref and path validation** (0021): ref names (`git check-ref-format` rules) and paths (no `..`, no empty segment, no NUL) in the shared validator. Tests: valid and invalid refs and paths, boundaries.
5. **`GitStore` with gix** (0003): bare init with `HEAD` on the default branch, refs, tree, blob, log, diff; git spawn for transport. Tests on a fixture repository; one gix / `git` binary parity test on refs.
6. **Ref resolution** (0021): `<ref>/<path>` resolution (longest branch, then tag, then full commit id) on `GitStore`. Tests: `feature/x/README.md` is branch `feature/x`, tag fallback, commit id, unknown ref.
7. **Repository reconciliation** (0012, 0026): at startup, matching by key: creation (init in `tmp/`, rename, commit), rename (owner/name updated, `public_id` and directory unchanged), redirects equal to `previous_names`, permissions and mirror configuration rows, archival (`archived_at` set, row, directory and redirects kept in place, permissions deleted, queued and running runs cancelled, mirrors stopped; a retained `pending_push` row stays), unarchival of the same key (same row, same directory, content intact); a declaration, rename or previous name taking a current or previous name of an archived repository aborts startup naming both keys; the orphan sweep counts every row, archived included; `repo-created`, `repo-renamed`, `repo-archived`, `repo-unarchived`. Tests: each case, mirror runtime state kept across a configuration rerun, no-op rerun writes nothing, an archived row keeps its directory, runs and mirror status, another key taking its name or one of its previous names is refused, its redirects still resolve, the same key re-declared unarchives the same row with its refs intact.
8. **Admission gate and push reconciliation** (0003): per-repository admission, `pending_push` row before any ref mutation, ordered conversion into `push` events after the mutating process exits, retained row on an unexpected value, recovery at startup and on a timer; no hook yet. Tests: applied, not applied, partial, creation and deletion; crash after ref update → one event after restart; admission refused while a row is pending.
9. **Admin commands** (0012, 0021, 0026): `admin repo list` maps public ids to names and marks archived repositories; the `admin repo create` placeholder is deleted. Tests: list output; `repo create` is an unknown subcommand.
10. **`admin import`** (0021, 0026): path, bundle or URL into a declared empty repository (never an archived one) through the gate of task 8, `refs/heads/*` and `refs/tags/*` only, `push` events by `system`, `repo-imported`. Tests: import preserves refs, tags and `HEAD`; undeclared target, non-empty target and missing default branch refused with nothing written; a crash during import is reconciled.
11. **`RepoService` Get/List** (0020, 0026): read-only, public repositories; List returns active and archived repositories, archived ones marked; Get by `public_id` or by name (the row holding the name, active or archived, then a redirect, which may lead to an archived repository), opaque-cursor pagination by name, `REPO_NOT_FOUND`; declared fields, permissions and mirror configuration in the response. Tests: pagination stability, redirect, archived repository by `public_id` and by name, not found, no mutating method in the service.
12. **Smart HTTP** (0003, 0005, 0021): `info/refs` and `git-upload-pack` through `git upload-pack --stateless-rpc`, redirects resolved transparently, clean 404, archived repository refused (403, "archived"); UI route `/<owner>/<name>` answers 301 for a previous name. Tests: clone through the router in a test, unknown repository 404, redirect.
13. **`/raw/`** (0005, 0021): `/raw/<owner>/<name>/<ref>/<path>` with task 6's resolution and task 4's validation. Tests: file, nested branch, tag, commit id, invalid path refused, missing file 404, archived repository refused (403).
14. **`nixos-git-http`** (0015, 0021, 0022, 0026): `alice/demo` declared in the test's NixOS configuration; `admin import` of a bundle → `git clone http://forge/alice/demo.git` identical (same refs, same `HEAD`); `nix flake prefetch git+http://forge/alice/demo.git` succeeds; `/raw/alice/demo/main/flake.nix` returns the file; `/raw/alice/demo/feature/x/README.md` resolves branch `feature/x`; clone of an undeclared repository → clean 404; a second import into `alice/demo` refused. Closing test: `nixos-git-http`.
15. **`nixos-repo-lifecycle`** (0012, 0021, 0026, 0032): configurations switched in the test (specialisations): rename with the old name in `previous_names` → old name clones, UI route 301, same directory; removal → repository archived, `repo-archived` logged, directory still in place and nothing in `tmp/orphans/`, clone and `/raw/` refused as archived (by name and by the old redirected name), `RepoService.Get` by `public_id`, by name and by the old name answers with the repository marked archived and its tree still browsable, UI route of the old name still 301; another key declared with the archived name, or with its old name → startup refused naming both keys; the original key re-declared → same `public_id`, unarchived, clone identical to the original with no `admin import`; a stray directory under `repos/` → moved to `tmp/orphans/` with `repo-orphaned`. Closing test: `nixos-repo-lifecycle`.

**Acceptance criteria**

- The two nixosTests above pass; every check of tasks 1–13 passes on all systems where it runs.
- No RPC, CLI command or admin command creates, renames or deletes a repository.

**Delegable**: repository fixtures, parity tests, route boilerplate, reconciliation cases.

---

## J3 — Writes over SSH, hooks, permissions, signatures

**Deliverable**: `git push` over SSH, controlled by permissions and ref rules, with mandatory signatures.

Content: dedicated sshd instance in the module, `AuthorizedKeysCommand` → `kajiba-server ssh-keys`, `kajiba-server serve-ssh` (0004); keys come from the configuration (0014), no key-mutating RPC; `kajiba-server hook pre-receive|post-receive` → `kajiba.hook.v1` over the local socket, global `core.hooksPath`, joining J2's admission gate and `pending_push` reconciliation (0003, 0026); read/write/admin RBAC + `is_admin`, repository permissions from the configuration (0014, 0026); ref rules of 0013/0014, fixed by the product, per-pattern rules declared only (0026); signature algorithm of 0014; `push` events (0017); `/readyz` now checks the sshd integration. `AuthorizedKeysCommand` prints only the canonical stored key (`ssh_key.public_key`, no comment), never the configured line.

**Acceptance criteria** (`nixos-git-ssh`, classic ed25519 keys)

- `write` user: push of signed commits to `refs/heads/main` accepted; `push` event present with principal, ref, old/new sha.
- Unsigned commit → refused with a message naming the commit; signed by a key that is not the pusher's → refused; lightweight tag → refused; signed annotated tag → accepted; first push to an empty repository checks every commit.
- `read` user → push refused; unknown key → connection refused by sshd.
- Push to `refs/contrib/x/y` or `refs/foo` → refused (prefix); force-push or deletion on `main` → refused for `write`, accepted for `admin` except deleting the default branch.
- Server stopped → push refused with an explicit message, no partial state.
- Server killed between ref update and `PostReceive` → after restart the `push` event exists exactly once; a second push to that repository is refused until reconciliation, then accepted, with both events in order. Cover partial success, creation, deletion, failed updates and a surviving `receive-pack`; an unexpected ref value retains its pending row and blocks admission.
- SSH command other than `git-upload-pack`/`git-receive-pack` → refused.
- Clone, fetch or push of an archived repository over SSH → refused with a message saying it is archived (0026).

**Delegable**: nixosTest scenarios, generation of key fixtures and signed commits, hook stdin parsing.

---

## J4 — Web and API authentication, minimal CLI

**Deliverable**: passkey login, personal tokens, protected gRPC/grpc-web API, a Rust CLI able to list repositories.

Content: WebAuthn enrollment through the `admin enroll` link and login (webauthn-rs), cookie sessions, lifecycle values of 0014; runtime PATs (create/list/revoke; declared ones read-only); single auth interceptor with per-method scopes; tonic-web + restricted CORS (0020); `RepoService` Get/List behind authentication, still read-only (0026); `kajiba` CLI (Rust): `login` (stores the PAT file), `repo list|get`, `--json`, version header and 0023 matrix (0007); Playwright harness and `nix run .#e2e` (0015) with a minimal login page ahead of J5's UI.

**Acceptance criteria**

- Unauthenticated call → `UNAUTHENTICATED` / `CREDENTIAL_INVALID`; missing scope → `PERMISSION_DENIED` / `SCOPE_MISSING`; modifying a declared token → `FAILED_PRECONDITION` / `DECLARED_IN_CONFIG`; a method without a scope option fails a unit test.
- `nixos-cli`: the CLI lists a declared repository with a declared PAT, then clones it (J2); expired PAT → refused; CLI of another `0.minor` → `VERSION_INCOMPATIBLE`.
- `e2e` (WebAuthn virtual authenticator): enrollment through the `admin enroll` link, logout, login again, session invalidated after expiry; a second `admin enroll` revokes the passkey; wrong-origin ceremony refused.
- `buf breaking` runs (informational before 1.0); `proto-generated` stays clean.

**Delegable**: clap skeleton, interceptor tests, e2e scenarios.

---

## J5 — Frontend v1

**Deliverable**: the embedded SvelteKit interface: repository list, tree/file navigation, history, refs, account page (passkeys, tokens).

Content: adapter-static SPA, connect-web client, bundle injected into the server build, `--ui-dir` for troubleshooting, Vite proxy and development origins (0006); UI routes from 0021; code rendering with highlighting, Markdown for READMEs.

**Acceptance criteria**

- `nix build .#kajiba-server` contains the bundle; the server serves the UI on `/` with no external directory.
- `e2e`: after login, browse `alice/demo`, open a file, view a branch's history, switch ref; declared keys and tokens, repository settings, permissions and mirror configuration shown read-only, with no edit control (0026).
- In the devShell, `pnpm dev` against a local server (passkey login) and against a remote URL (PAT injected by the proxy) works without recompiling Rust (documented in CONTRIBUTING).
- Budget: initial bundle < 300 KB compressed, measured in CI.

**Delegable**: repetitive components, list pages, e2e tests.

---

## J6 — Runners and CI by Nix convention

**Deliverable**: a runner declared in Nix runs `nix flake check` on every trusted push; live logs in the UI and the CLI; manual trigger.

Content: `kajiba.runner.v1` bidirectional stream, declarative enrollment by shared secret, runner rows and capabilities declared in the server configuration (0026), attempts, server-owned leases, sequenced logs, reconnection (0008); `kajiba-runner` with the `Executor` trait and the `nix` backend, runner-host requirements checked at startup (0009); `run/job/job_attempt/step/log_chunk` model; `ci` bus handler: push on a trusted ref by `local` → run; manual `TriggerRun(sha)`; `RunService` with log streaming; CLI `ci run|watch|logs`; UI: run list, run page with logs; short retention (0010); `nixosModules.runner`; `darwinModules.runner` exported and evaluated by `module-eval` (darwin execution is J13).

**Acceptance criteria** (`nixos-ci`, nodes `forge` + `runner`, Nix closure preloaded)

- The runner connects with the declared secret, shows up with its declared capabilities; wrong secret → refused; a capability beyond the declaration → refused (`runner-refused`, reason `capabilities`); different `0.minor` → refused (0023); a runner host with `sandbox = false` refuses to start.
- Signed push on `main` of a repository with CI enabled → run created, executed, success; failing `flake check` → run failed with logs.
- Push on `refs/contrib/*` (injected in test by the forge) → no run; manual `TriggerRun` on that sha → run.
- `kajiba-server` restart during a job → the runner reconnects, the job finishes, the result is recorded exactly once; no log chunk lost or duplicated.
- Runner killed during a job → lease expired → attempt `lost`, then a new attempt on a compatible runner if replayable; a late result from the old attempt is ignored.
- Job requiring an absent backend → stays "pending" with a visible reason.
- CLI `ci watch` shows the stream and exits with the run's code; `e2e`: the run page shows live logs.
- Runner journal passes `nisshi validate --logs`.

**Delegable**: test machinery (minimal flake with a trivial check, store preloading), lease/reconnection tests, proto message boilerplate.

---

## J7 — Mirroring to GitHub

**Deliverable**: declared mirrors are pushed to GitHub, queued, with unlimited retries and reconciliation; GitHub targets are created and kept configured (0027).

Content: mirrors from the configuration (0026), read-only `MirrorService` (status), `mirror` bus handler, coalesced queue in the DB, capped backoff, reconciliation timer, explicit refspecs with scoped `--prune` (0011); GitHub target preparation (create if missing, description, homepage, visibility, archived, issues/wiki/projects/discussions/pull requests/Actions disabled; never delete or rename), `github` mirror fields, `mirror.prepared_digest`, `mirror-prepared` (0027); the token permissions marked "to verify in J7" in 0027 checked first; per-mirror status in the UI and `kajiba mirror status`.

**Acceptance criteria** (`nixos-mirror`, node `remote` = bare repo behind sshd or HTTP basic, simulating GitHub)

- Push → `remote` up to date within N seconds; branch and tag deletion and force-push propagated; tags propagated.
- `refs/contrib/*` never present on `remote`; a ref outside `refs/heads`/`refs/tags` created on `remote` is left untouched.
- `remote` unreachable → attempts with backoff, "failed" status visible; `remote` back → convergence without intervention; five pushes during the outage → a single mirror job at recovery.
- Reconciliation timer: ref modified by hand on `remote` → corrected on the next pass.
- Mirror enabled on a `remote` already identical → no ref updated.
- Credential absent or unreadable → explicit error in the status, no crash.
- GitHub target preparation against a fake GitHub API on `remote`: missing repository created, then pushed; drifted description, visibility or archived corrected on the next reconciliation pass; Actions and pull requests disabled; no delete or rename call ever sent; preparation failure → mirror error, no push; archived target prepared, never pushed.

**Delegable**: test `remote` node, outage simulation, coalescing tests.

---

## J8 — Operations: backup, timers, complete module

**Deliverable**: the forge is operable day to day: dump/restore tested, automatic cleanups, complete and documented NixOS module.

Content: `kajiba-server dump` under the write barrier, `restore`, `dump.keep` (0016); gc / retention / reconciliation / dump timers (0022); configuration reload on `SIGHUP` without restart (new ADR to write: reload semantics and the settings that still require a restart, such as listen addresses, state directory and metrics listen); module options frozen for J9; operations documentation in the README.

**Acceptance criteria** (`nixos-ops`)

- Dump → new empty VM → restore → identical `git clone`, users, passkeys and runtime PATs working, historical runs visible.
- Dump closes admission while an admitted push and another mutating operation are in flight → both drain, pending push events commit before the snapshot, restored refs and DB agree. New push or mutation during the barrier → refused with the maintenance message, accepted after.
- Retention timer: logs older than the retention deleted, DB index consistent; `dump.keep` honored.
- Weekly `git gc` executed without breaking a concurrent push.
- Configuration-only change (a repository or user added) → `nixos-rebuild switch` reloads the service instead of restarting it (`Type=notify-reload`, `reloadIfChanged`): the new repository is clonable, a clone in progress completes, the process id is unchanged. Invalid configuration on reload → refused with a log line, previous configuration kept, service still ready. A change to a restart-only setting → reload refused with a message naming the setting.

**Delegable**: scripted restore procedure, timer tests.

---

## J9 — Production readiness alongside an existing forge

**Deliverable**: everything a deployment needs to run kajiba next to another forge on the same host, behind a TLS-terminating reverse proxy, with no port, user or path collision.

Content: deployment guide in the README (module options, persisted paths for an ephemeral root, reverse proxy requirements of 0005 with genkan as example, secrets as files, `admin enroll` procedure, dump directory to copy off-host); every listener and the sshd port configurable; defaults that cannot collide with a host's admin sshd.

**Acceptance criteria** (`nixos-production`, nodes `proxy` (TLS reverse proxy with a test CA), `forge`, `runner`, `client`)

- From `client`: HTTPS clone through `proxy`, SSH push on the configured port, CI run visible, gRPC CLI call over TLS, a grpc-web call and a log stream through the proxy.
- `forge` reboot with only the declared persisted paths kept (root wiped) → everything persists.
- Service restart during a run → run finishes, recorded once.
- Another service already bound to port 22 and 3000 on `forge` → kajiba unaffected.

**Delegable**: the guide, the test topology.

---

## J10 — Import from another forge via `admin import`

**Deliverable**: repositories of an existing forge move to kajiba with their history, their GitHub mirrors are adopted without disruption, and flakes resolve their inputs from kajiba with the mirror as fallback; a deployment that used koseki can stop using it at the end of this milestone (0027).

Content: `admin import` from a URL into declared empty repositories (0026; all branches and tags, annotated tags kept), batch import from a list; mirror adoption (0011); documented URL change for flake inputs (0021); repositories that need private visibility wait for private repositories (0013); koseki migration note (each entry → a repository and mirror declaration, 0027): a deployment migrates its entries and stops using koseki at the end of the milestone (resources released from its state without destroy; until then it declares no `github` table for a koseki-managed repository).

**Acceptance criteria** (`nixos-import`, node `oldforge` = git server simulating the previous forge, `remote` = GitHub simulation)

- Every imported repository, declared in the configuration beforehand: same refs, same HEAD and same tags as on `oldforge`.
- Mirror enabled after import on a `remote` already in sync → no ref updated, no force-push.
- `nix flake prefetch` from kajiba and from `remote` give the same `narHash`.
- A push on kajiba after import triggers CI and reaches `remote`.
- A set of koseki-shaped entries translated per 0027 passes `config check`; their mirrors adopt existing targets with no repository created.

**Delegable**: import script, ref comparison.

---

## J11 — Pipeline file and nspawn backend

**Deliverable**: isolated impure steps, a `.kajiba/pipeline.toml` file, a static site deployed by CI.

Content: TOML format (jobs, `needs`, runner requirements, minimal matrix) (0010); `nspawn` backend with network and secrets injected on the runner side from files (0009); out-of-forge secrets documented; example pipeline building a static site and deploying it to a web server node.

**Acceptance criteria** (`nixos-pipeline`)

- `nspawn` job with network access to a test node, ephemeral files after the job; job without declared network → network cut; injected secret visible in the job, absent from the logs.
- Invalid pipeline file → run fails immediately with a line/column diagnostic; no file → `nix flake check` convention unchanged.
- Site deployed by a push on `main`; a failing `nix flake check` blocks the deployment (`needs`).

**Delegable**: TOML parser/validator and its tests, nspawn tests.

---

## J12 — Self-hosting: the forge builds and deploys itself

**Deliverable**: kajiba's own repository on kajiba; its pipeline checks it and redeploys the instance it runs on.

Content: pipeline that, after `flake check`, deploys a new forge generation (J11 impure step on a trusted runner, restart tolerance 0008); fallback source for consumers (0015); operations README up to date.

**Acceptance criteria** (`nixos-self-deploy`)

- A push on `main` of the hosted kajiba repository deploys a new generation of `forge` without intervention; the run is recorded successful after the restart.
- A broken generation pushed → the deployment job fails, the running instance stays up.
- The repository's flake resolves from kajiba and from the mirror with the same `narHash`.

---

## After the core phase (sketch, to detail when the time comes)

- **J13 — macOS runner**: `darwinModules.runner` in use, `process` backend, first darwin jobs.
- **J14 — Issues**: model, API, UI, Markdown, cross-references commits/issues.
- **J15 — GitHub App and OAuth**: external identity (`user.kind = external`, zero rights), mirror authenticated by the App (end of the PAT), automatic closing of PRs on the mirrors (0011).
- **J16 — External merge requests**: submission (repository + branch), fetch into `refs/contrib/<user>/<branch>`, review, manual CI only, decision on UI merge (0014 open).
- **J17 — microVM**: spike on a host with virtualization support; `microvm` backend.
- **J18 — git-LFS**: authenticated HTTP write endpoint, storage, retention.
- **Elsewhere**: organizations (non-user owner), private repositories and branches, ntfy notifications, device flow for the CLI, interactive runner enrollment.
