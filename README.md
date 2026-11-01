# kajiba (鍛冶場, "the forge")

A self-hosted git forge: repository hosting over SSH and HTTP, CI through a
small runner protocol with interchangeable execution backends, one-way
mirroring to GitHub, a web UI, and a CLI. One owner to start, built to grow
into a small team without a rewrite; deployed through Nix only. See [`docs/adr/`](docs/adr/) for the
design decisions and [`docs/milestones.md`](docs/milestones.md) for the plan.

Status: **J8 operations integration** — repository hosting, authentication,
UI, CI and mirroring are implemented. The final disaster-recovery acceptance
check remains in J8 task 13. See the milestone plan for verification status.

## Layout

- `proto/` — `.proto` files, the source of truth for the API and the
  forge↔runner protocol.
- `crates/` — shared Rust libraries (no binary): `kajiba-core`, `kajiba-gitstore`,
  `kajiba-proto`, `kajiba-exec`.
- `apps/` — the five deliverables, one subfolder each:
  - `server/` — `kajiba-server`, the Rust binary.
  - `git/` — `kajiba-git`, the repository writer, daemon and SSH helpers.
  - `runner/` — `kajiba-runner`, the Rust binary.
  - `cli/` — `kajiba-cli`, the Rust CLI (binary `kajiba`).
  - `ui/` — SvelteKit SPA, `kajiba-ui`, embedded into `kajiba-server` at
    build time from J5 on.
- `nix/` — packages, NixOS/nix-darwin modules, tests.
- `docs/adr/` — architecture decision records.
- `pnpm-workspace.yaml` — workspace marker at the repo root for `apps/ui`
  (one member; mostly so that tools that detect a project by its manifest
  file see Node from the repo root).

## Building

```sh
nix develop            # devShell: Rust, pnpm, buf, protoc, sqlx-cli, mold
nix flake check        # unit + integration checks
nix build .#kajiba-server .#kajiba-git .#kajiba-runner .#kajiba-cli .#kajiba-ui
```

`nix flake check` is only complete on Linux (`aarch64-linux`); the NixOS
integration tests don't run on Darwin.

The frozen NixOS option surface and unit contracts are documented in
[`docs/nixos.md`](docs/nixos.md).

## NixOS deployment

Import `kajiba.nixosModules.server` from the kajiba flake in the deployment
configuration and set `services.kajiba-server.package`,
`services.kajiba-git.package` and `services.kajiba-server.ui.package` to
`kajiba.packages.${pkgs.stdenv.hostPlatform.system}.{kajiba-server,kajiba-git,kajiba-ui}`
respectively (or supply those packages through an overlay). Add this module:

```nix
# Import kajiba.nixosModules.server and provide its server/git/ui packages first.
{
  services.kajiba-server = {
    enable = true;
    http.publicUrl = "https://forge.example.org";
    users.alice.isAdmin = true;
    repos.demo = {
      owner = "alice";
      description = "A demonstration repository";
    };
    dump = {
      dir = "/var/lib/kajiba-dumps";
      keep = 7;
    };
  };
}
```

This is [`docs/examples/nixos.nix`](docs/examples/nixos.nix), evaluated by
`module-eval`. Put a TLS-terminating reverse proxy in front of the loopback HTTP
listener; SSH uses the dedicated port 2222. Declare SSH/signing keys and
credentials in the deployment configuration before allowing writes.
[`docs/nixos.md`](docs/nixos.md) lists the complete typed server, Git and runner
options, defaults, secret validation and unit contracts. Runner hosts import
`kajiba.nixosModules.runner` and supply the runner package and mTLS identities.

Persist `services.kajiba-server.persistedPaths`: the entire server state directory
(default `/var/lib/kajiba-server`), Git state directory (`/var/lib/kajiba-git`)
and `dump.dir`. Persist runner state separately for job recovery. Server state
and dumps belong to `kajiba-server:kajiba-server`; Git state belongs to
`kajiba-git:kajiba-git`, directories mode 0750. The server has Git group read
access, never repository write access. Do not flatten these ownerships.

Configuration and secret files are **deployment-owned**, outside dumps. Keep the
flake, declared identities/PAT hashes and secret-manager material recoverable
separately. Generated `/etc/kajiba/{server,git,runner}.toml` contains paths, not
secrets. Supply runtime secret files with the service ownership and restricted
permissions described in the option reference; never put secrets in the Nix store.

## Dump and external backup

As root or `kajiba-server`, with the server running:

```sh
kajiba-server admin dump
```

Success prints one completed directory under the running configuration's
`dump.dir`. Publication follows SQLite backup-API snapshotting, inventory/SHA-256,
SQLite and Git verification. `manifest.json` records format/product/schema
versions, date and all payload files. Dumps contain the database, every registered
bare repository (including archives) and both orphan quarantines with their
identity reservations. They exclude CI bundles, logs/artifacts, caches, scratch,
repository scratch lock files/sockets, deployment configuration and secrets.
Quarantine names ending in `.lock` are preserved as identity reservations. Historical runs and steps
survive restore; excluded logs remain unavailable and active worktree runs without
bundles fail `bundle-missing`. Preserve quarantines; there is no automatic deletion.
Dumps contain credential hashes and private repository data: protect copies as state.

During maintenance new mutations return `UNAVAILABLE` / `MAINTENANCE`; pushes
print a retry message. Retry after the dump finishes. Reads and clones continue;
admitted mutations and push reconciliation drain before copying. New runner
assignments and lease expiry pause, acknowledgments wait for persistence, and
running job units continue: dumping never waits for whole builds. Failure releases
maintenance and preserves good dumps. Success emits `dump-done{dir,dur_us}` and
updates `kajiba_dump_last_success_timestamp_seconds`.

`kajiba-dump.timer` runs daily and retains the newest `dump.keep` verified dumps
(default 7; zero still keeps the new dump). Failed creation never prunes.
`kajiba-gc.timer` runs weekly through `kajiba-git gc` and the same admission gate.
Both timers are persistent; missed invocations run after boot. Test/manual commands:

```sh
systemctl start kajiba-dump.service
systemctl start kajiba-gc.service
```

CI retention runs in-process at `ci.runAt` (default 03:00), with default log/artifact
retention 14d and worktree retention 1d. There is no `kajiba-retention.timer`.

An external tool's pre-backup hook must request a fresh dump, wait for success,
and finish copying under `dump.dir/.copy.lock` (ADR 0174). This complete example
runs as root and stages a private copy in a deployment-provisioned handoff directory
outside `dump.dir`, owned by `kajiba-server`, mode 0700:

```sh
runuser -u kajiba-server -- sh -eu <<'SH'
umask 077
dump_root=/var/lib/kajiba-dumps
handoff=/var/lib/kajiba-backup-handoff
exec 9>"$dump_root/.copy.lock"
flock -x 9
dump=$(kajiba-server admin dump)
cp -a -- "$dump" "$handoff/"
SH
```

The script is also available as [`docs/examples/backup-hook.sh`](docs/examples/backup-hook.sh).
The external tool consumes the copied directory **only if the hook succeeds**.
The handoff path, its retention and off-host transfer are deployment concerns;
never use a live state directory as a backup source. On copy failure remove only
the incomplete private handoff copy before retrying. For a custom `dump.dir`, change
`dump_root` to the running value. Serialize any handoff cleanup with the external
reader. If the tool reads `dump.dir` directly instead, hold descriptor 9 until
its entire read finishes; a pre-hook that releases the lock before that read
cannot protect it.

Rotation tries the same exclusive lock without waiting and skips pruning while
it is held, including during the hook's dump request. Later successful dumps
retry retention. A stalled copy can consume disk space but does not prolong
maintenance. Never unlink or replace `.copy.lock`; process exit releases its flock.
Ignore `.partial-*` and copy only the returned completed directory, preserving all
payload content and `manifest.json`. Other deletion tools must honor this lock.
No backup-tool-specific option or dependency is required.

## Offline restore and service recovery

The following Linux/root procedure is exercised by `nixos-ops-restore` and the
`nixos-ops` disaster-recovery check. Recover deployment configuration,
service users and secrets first; use a supported server version (newer dump schemas
are refused). Keep the input dump outside both destination trees and protect it
from external deletion throughout restore. Stop timers, both daemons and **all
activating sockets**; stopping services alone leaves sockets able to restart them:

```sh
systemctl stop kajiba-dump.timer kajiba-gc.timer kajiba-dump.service kajiba-gc.service
systemctl stop kajiba-server.service kajiba-git.service kajiba-server.socket kajiba-server-runner.socket kajiba-git.socket kajiba-git-ssh.socket
```

Omit `kajiba-server-runner.socket` if no runner listener is configured. Verify all
listed units are inactive (or failed), with no pending starts. Existing independent
runner jobs can continue; coordinate runner state with the recovery point before
reconnecting them. Stopping a runner service does not stop its Linux job units.

Restore requires **two empty, disjoint state directories**, except their existing
regular lifetime lock files. Never remove those lock files, overwrite non-empty
state, or wipe a working instance merely to try a backup. Move/retain old state
through a separate operator recovery plan. Provision missing directories with
service ownership (substitute configured paths):

```sh
install -d -o kajiba-server -g kajiba-server -m 0750 /var/lib/kajiba-server
install -d -o kajiba-git -g kajiba-git -m 0750 /var/lib/kajiba-git
kajiba-server restore /path/to/completed-dump --config /etc/kajiba/server.toml
```

Restore holds both lifetime locks, validates the input, stages and revalidates both
trees before installation, sets service ownership (directories 0750, files 0640),
and clears excluded log indexes. Failure rolls back only what restore created;
input and pre-existing content remain. It does not migrate or start services.
Power loss between tree switches requires manual inspection of partial restored
content while units stay stopped, before retrying. On success:

```sh
systemctl reset-failed kajiba-server.service kajiba-git.service kajiba-server.socket kajiba-server-runner.socket kajiba-git.socket kajiba-git-ssh.socket
systemctl start kajiba-git.service kajiba-server.service
curl -fsS http://127.0.0.1:8080/readyz
systemctl start kajiba-dump.timer kajiba-gc.timer
```

Again omit an unconfigured runner socket. Normal startup applies migrations and
reconciliation. Check repository refs/HEAD and HTTP/SSH clones, identities,
authentication, historical run states and a fresh signed push/CI run against the
recovery point. `nixos-ops` covers the full disaster-recovery assertions.

For ordinary changes, NixOS reloads server declarations (`users`, `runners`,
`repos`, `mirror`) with SIGHUP; `systemctl reload kajiba-server.service` only
confirms signal delivery. Check `config-reloaded` / generation or
`config-reload-failed`; `config-reload-partial` means restart-only values were
retained. Packages, UI and other settings need a restart. Git has no reload.
Ordinary restarts retain systemd sockets; server streams end promptly and other
requests drain for up to 5s, Git sessions for up to 30s (35s unit bound). Longer
operations may need retry; runner jobs survive Linux runner-service restarts.

After a startup failure, repair the cause before resetting the affected service
and its sockets, then start the service as above. Five starts in five minutes
exhaust the budget and fail activating sockets too; waiting alone does not revive
failed sockets. Inspect structured stderr with
`journalctl -u kajiba-server -u kajiba-git -o cat` and loopback metrics when enabled.

## License

Apache-2.0, see [`LICENSE`](LICENSE).
