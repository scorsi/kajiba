# kajiba — milestone plan

Technical milestones, no calendar. Each milestone has a verifiable **deliverable**, explicit **acceptance criteria**, the **tests** that prove them in `nix flake check`, and the **delegable** part (tests, boilerplate, refactoring) I can take. A milestone is closed when its criteria pass in CI on `aarch64-linux`; a milestone never leaves `nix flake check` red in between.

ADR references in parentheses.

---

## J0 — Skeleton: a green repository at the first commit

**Deliverable**: the `kajiba` monorepo (0018) hosted on Forgejo with a GitHub mirror, `nix flake check` green, no feature.

Content: flake (kiso flakeModule, flake-parts), devShell (stable Rust, Go, pnpm, buf, protoc, sqlx-cli, mold, playwright), Cargo workspace with the six empty crates, empty `kajiba-cli` Go module, empty SvelteKit project, `proto/` with `kajiba/v1/health.proto` and `buf.yaml`, `nix/` with packages, `nixosModules.{server,runner}`, `darwinModules.runner` and a trivial `nixosTest`, `docs/adr/`, `LICENSE` (Apache-2.0), `README.md`, `CLAUDE.md`, `CONTRIBUTING.md` (basics, in English), treefmt.

**Acceptance criteria**

- `nix flake check` green on `aarch64-linux` (Forgejo Actions); green on `aarch64-darwin` for the non-VM checks.
- `nix build .#kajiba-server .#kajiba-runner .#kajiba-cli .#kajiba-ui` produce artifacts; `kajiba-server --version` prints the workspace version.
- The "identical versions" check (Cargo, Go, package.json) passes (0023).
- `buf lint` and `buf generate` run; the generated Rust/Go/TS code for `health.proto` compiles in all three clients.
- The `nixosTest` starts `kajiba-server` through the module, `/healthz` answers 200.
- GitHub mirror in place (through Forgejo for now), GitHub repo configured per 0011.

**Delegable**: all the Nix/buf/treefmt boilerplate, the three code skeletons, the README/CLAUDE.md/CONTRIBUTING basics.

---

## J1 — Server core: config, database, events

**Deliverable**: a server that starts from a Nix-generated configuration, migrates its database, logs, and knows how to create its first administrator.

Content: TOML config struct + `kajiba config check` (0019); SQLite through sqlx, embedded migrations, complete v0 schema from 0013 (empty tables accepted); append-only `event` + internal bus with replay (0017); `tracing` → journald, correlation identifiers, `/readyz` (0022); `kajiba admin bootstrap` creating the first `is_admin` user and a single-use WebAuthn enrollment link (0014); gRPC reflection outside production; NixOS module: `stateDir`, sub-paths to persist, systemd hardening, `ExecStartPost` on `/readyz`.

**Acceptance criteria**

- `nixosTest`: service ready (`/readyz` 200); VM restart with persisted `stateDir` → data intact; `stateDir` wiped → clean re-migration.
- The configuration produced by the module passes `kajiba config check`; a config with an inline secret is refused.
- An event written then the process killed before dispatch is replayed at restart (Rust integration test).
- `journalctl -u kajiba` shows structured lines with the request identifier.

**Delegable**: SQL migrations, fixtures, replay tests, NixOS module options.

---

## J2 — Read-only git hosting

**Deliverable**: bare repositories created by the forge, clonable over smart HTTP, browsable through the API; Nix can use them as inputs.

Content: `RepoService` (Create/Get/List, owner = user); `repos/<owner>/<name>.git` layout (0012, 0021); `GitStore` boundary with gix (refs, tree, blob, log, diff) and git spawn for transport (0003); smart HTTP and `/raw/` routes (0005, 0021); `kajiba admin import <owner>/<name> <path-or-bundle>` (also used for the migration, J10); forbidden names and rename redirects (0021).

**Acceptance criteria**

- `nixosTest`: import of a bundle → `git clone http://forge/scorsi/demo.git` identical (same HEAD); `nix flake prefetch git+http://forge/scorsi/demo.git` succeeds; `/raw/scorsi/demo/main/flake.nix` returns the file; `git clone` of a nonexistent repository → clean 404.
- Rename → old name clones successfully, UI answers 301.
- `GitStore` unit tests: tree, blob, log, diff on a fixture repository; one gix / `git` binary parity test on refs.

**Delegable**: repository fixtures, parity tests, route boilerplate.

---

## J3 — Writes over SSH, hooks, permissions, signatures

**Deliverable**: `git push` over SSH, controlled by permissions and ref rules, with mandatory signatures.

Content: dedicated sshd instance in the module, `AuthorizedKeysCommand` → `kajiba ssh-keys`, `kajiba serve-ssh` (0004); `SshKeyService`; `kajiba hook pre-receive|post-receive` → `kajiba.hook.v1` over a Unix socket, global `core.hooksPath` (0003); read/write/admin RBAC + `is_admin` (0014); ref rules (0013); signature verification through `allowed_signers`; `push` event emitted by post-receive (0017).

**Acceptance criteria** (`nixosTest`, classic ed25519 keys)

- `write` user: push of signed commits to `refs/heads/main` accepted; `push` event present with principal, ref, old/new sha.
- Unsigned commit → refused with a message naming the commit; signed by an unregistered key → refused.
- `read` user → push refused; unknown key → connection refused by sshd.
- Push to `refs/contrib/x/y` or `refs/foo` → refused (prefix); force-push on `main` → refused for `write`, accepted for `admin` (v0 rule).
- Server stopped → push refused with an explicit message, no partial state.
- SSH command other than `git-upload-pack`/`git-receive-pack` → refused.

**Delegable**: nixosTest scenarios, generation of key fixtures and signed commits, hook stdin parsing.

---

## J4 — Web and API authentication, minimal CLI

**Deliverable**: passkey login, personal tokens, protected gRPC/grpc-web API, a CLI able to create a repository.

Content: WebAuthn enrollment + login (webauthn-rs), cookie sessions (0014, 0020); hashed PATs with scopes and expiration; single auth interceptor (cookie or bearer); tonic-web + restricted CORS; `kajiba-cli` Go: `login` (PAT), `repo list|create`, `--json` (0007); 0020 conventions (errors, pagination, ULID).

**Acceptance criteria**

- Unauthenticated call → `UNAUTHENTICATED` with `ErrorInfo`; missing scope → `PERMISSION_DENIED`.
- `nixosTest`: the CLI creates a repository with a PAT, lists it, then clones it (J2); expired PAT → refused.
- Playwright e2e (WebAuthn virtual authenticator): enrollment through the bootstrap link, logout, login again, session invalidated after expiration.
- `buf breaking` runs (informational in v0).
- `buf generate` → the Go client and the TS client are regenerated without manual diff.

**Delegable**: generated Go client and cobra skeleton, interceptor tests, e2e scenarios.

---

## J5 — Frontend v1

**Deliverable**: the embedded SvelteKit interface: repository list, tree/file navigation, history, refs, account page.

Content: adapter-static SPA, connect-web client, bundle injected into the server build, `--ui-dir` for troubleshooting, Vite + grpc-web proxy in dev (0006); UI routes from 0021; code rendering with highlighting, Markdown for READMEs.

**Acceptance criteria**

- `nix build .#kajiba-server` contains the bundle; the server serves the UI on `/` with no external directory.
- Playwright e2e: after login, browse `scorsi/demo`, open a file, view a branch's history, switch ref.
- In the devShell, `pnpm dev` pointed at a local server or at the real instance's URL works without recompiling Rust (documented in CONTRIBUTING).
- Lighthouse not required; budget: initial bundle < 300 KB compressed (to adjust, but measured in CI).

**Delegable**: repetitive components, list pages, e2e tests.

---

## J6 — Runners and CI by Nix convention

**Deliverable**: a runner declared in Nix runs `nix flake check` on every trusted push; live logs in the UI and the CLI; manual trigger.

Content: `kajiba.runner.v1` bidirectional stream, declarative enrollment by shared secret, capabilities, lease/heartbeat, reconnection (0008); `kajiba-runner` with the `Executor` trait and the `nix` backend (0009); `run/job/step/log_chunk` model; `ci` bus handler: push on a trusted ref by `local` → run; manual `TriggerRun(sha)`; `RunService` with log streaming; CLI `ci run|watch|logs`; UI: run list, run page with logs; short retention (0010); `nixosModules.runner` + `darwinModules.runner`.

**Acceptance criteria** (`nixosTest` nodes `forge` + `runner`, Nix closure preloaded)

- The runner connects with the declared secret, shows up with its capabilities; wrong secret → refused; different major version → refused (0023).
- Signed push on `main` of a repository with CI enabled → run created, executed, success; failing `flake check` → run failed with logs.
- Push on `refs/contrib/*` (injected in test by the forge) → no run; manual `TriggerRun` on that sha → run.
- `kajiba-server` restart during a job → the runner reconnects, the job finishes, the result is recorded exactly once.
- Runner killed during a job → lease expired → job marked lost, then replayed on a compatible runner if replayable.
- Job requiring an absent backend → stays "pending" with a visible reason.
- CLI `ci watch` shows the stream and exits with the run's code; e2e: the run page shows live logs.

**Delegable**: test machinery (minimal flake with a trivial check, store preloading), lease/reconnection tests, proto message boilerplate.

---

## J7 — Mirroring to GitHub

**Deliverable**: flagged repositories are mirrored to GitHub, queued, with unlimited retries and reconciliation.

Content: `MirrorService` (per-repo enablement, remote, credential by sops path), `mirror` bus handler, coalesced queue in the DB, capped backoff, reconciliation timer, explicit refspecs (0011); per-mirror status in the UI and `kajiba-cli mirror status`; fine-grained PAT in sops for production.

**Acceptance criteria** (`nixosTest` node `remote` = bare repo behind sshd or HTTP basic, simulating GitHub)

- Push → `remote` up to date within N seconds; branch deletion and force-push propagated; tags propagated.
- `refs/contrib/*` never present on `remote`.
- `remote` unreachable → attempts with backoff, "failed" status visible; `remote` back → convergence without intervention; five pushes during the outage → a single mirror job at recovery.
- Reconciliation timer: ref modified by hand on `remote` → corrected on the next pass.
- Credential absent or unreadable → explicit error in the status, no crash.

**Delegable**: test `remote` node, outage simulation, coalescing tests.

---

## J8 — Operations: backup, timers, complete module

**Deliverable**: the forge is operable day to day: dump/restore tested, automatic cleanups, complete and documented NixOS module.

Content: `kajiba dump` and restore procedure (0016); gc / retention / reconciliation / dump timers (0022); module options frozen for J9; operations documentation in the README.

**Acceptance criteria**

- `nixosTest`: dump → new empty VM → restore → identical `git clone`, working users and PATs, historical runs visible.
- Retention timer: logs older than the retention deleted, DB index consistent.
- Weekly `git gc` executed without breaking a concurrent push (test: push during gc).
- Integration into sekkeizu of the rrsync pull to jiban, prepared (Nix file ready, not enabled).

**Delegable**: scripted restore procedure, timer tests.

---

## J9 — Deployment on ishizue, alongside Forgejo

**Deliverable**: kajiba runs on ishizue (ports distinct from Forgejo), runner on ishizue, real mirror to a test GitHub repository; kajiba itself stays hosted on Forgejo.

Content: flake import into sekkeizu, sops secrets (mirror PAT, runner secret), Tailscale certificates through the reverse proxy, impermanence persistence, rrsync pull enabled.

**Acceptance criteria** (manual verification, recorded in sekkeizu)

- From jiban: HTTP clone over Tailscale, SSH push with YubiKey (one touch), CI run visible, GitHub mirror up to date.
- ishizue reboot → everything persists; sekkeizu `switch` during a run → run finished.
- Daily dump pulled to jiban; restore tested once on a throwaway VM.
- Nothing in Forgejo is affected (ports, backups, runners).

**Delegable**: the Nix configuration on the sekkeizu side, the checklist.

---

## J10 — Repository migration and self-hosting

**Deliverable**: kiso, kanna, sekkeizu, sekkeizu-private, rirekisho and kajiba live on kajiba; sekkeizu consumes its inputs from kajiba; kajiba is hosted by itself.

Content: `admin import` from the Forgejo clones (history intact); GitHub mirrors taken over by kajiba (Forgejo stops pushing); URL change of the `scorsi-*` inputs (0021); `via-github.nu` revalidated; kajiba's CI on kajiba; extended fine-grained GitHub token, Forgejo runners removed from these repositories.

**Acceptance criteria**

- Every migrated repository: same HEAD and same tags as on Forgejo; GitHub mirror unchanged (no spurious force-push).
- `nix flake update` in sekkeizu resolves all inputs from kajiba; `nu scripts/via-github.nu` gives the same narHashes.
- A push on kajiba triggers kajiba's CI on kajiba, green.
- Recovery scenario documented and rehearsed once: kajiba broken → rebuild of sekkeizu through GitHub.

**Delegable**: migration script, ref comparison, input updates.

---

## J11 — Pipeline file and nspawn backend

**Deliverable**: isolated impure steps, a `.kajiba/pipeline.toml` file, rirekisho deployed by CI.

Content: TOML format (jobs, `needs`, runner requirements, minimal matrix) (0010); `nspawn` backend with network and secrets injected on the runner side by sops (0009); out-of-forge secrets documented; rirekisho pipeline (Astro + Svelte build, deployment to Caddy).

**Acceptance criteria**

- `nixosTest`: `nspawn` job with network access to a test node, ephemeral files after the job; job without declared network → network cut; injected secret visible in the job, absent from the logs.
- Invalid pipeline file → run fails immediately with a line/column diagnostic; no file → `nix flake check` convention unchanged.
- rirekisho deployed by a push on `main`; a failing `nix flake check` blocks the deployment (`needs`).

**Delegable**: TOML parser/validator and its tests, nspawn tests.

---

## J12 — Retiring Forgejo

**Deliverable**: Forgejo no longer exists in sekkeizu; kajiba deploys itself.

Content: kajiba pipeline that, after `flake check`, triggers the deployment on ishizue (J11 impure step, with restart tolerance 0008); removal of the Forgejo service, its backups and its mirror; ports `:443`/`:2222` taken over by kajiba.

**Acceptance criteria**

- A push on kajiba's `main` updates the production instance without intervention, the run finishes successfully after the restart.
- No reference to Forgejo in sekkeizu; backups and reminders (PAT expiration) transferred.
- Operations README up to date.

---

## After the core phase (sketch, to detail when the time comes)

- **J13 — macOS runner on jiban**: `darwinModules.runner`, `process` backend, first darwin jobs.
- **J14 — Issues**: model, API, UI, Markdown, cross-references commits/issues.
- **J15 — GitHub App and OAuth**: external identity (`user.kind = external`, zero rights), mirror authenticated by the App (end of the PAT), automatic closing of PRs on the mirrors (0011).
- **J16 — External merge requests**: submission (repository + branch), fetch into `refs/contrib/<user>/<branch>`, review, manual CI only, decision on UI merge (0014 open).
- **J17 — microVM**: nested virt spike on the current host or wait for the mini-PCs; `microvm` backend.
- **J18 — git-LFS**: authenticated HTTP write endpoint, storage, retention.
- **Elsewhere**: organizations (non-user owner), private branches, ntfy notifications, `/metrics`, device flow for the CLI, interactive runner enrollment.
