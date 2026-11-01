# NixOS module option reference

The J8 option surface below is frozen for J9. It describes the existing typed
modules, not additional configuration. Import `nixosModules.server` for the
paired server and Git daemon and `nixosModules.runner` on runner hosts. Supply
the corresponding flake packages through each `package` option (and
`services.kajiba-server.ui.package`). The modules have no raw `settings` option.

Names below are Nix option names. Generated TOML uses snake_case and is checked
at build time by the configured binary's `config check`. Durations are strings
`<n>s|m|h|d`; scopes, names, URLs and duration bounds use the Rust validator.
Unless marked required, values have the effective defaults listed below. Most
optional scalar/list options default to `null` in Nix: omission keeps the Rust
default. Setting an explicit value writes it to TOML.

## `services.kajiba-server`

| Option                                    | Type                                | Effective default / meaning                                                   |
| ----------------------------------------- | ----------------------------------- | ----------------------------------------------------------------------------- |
| `enable`                                  | boolean                             | `false`; enables both daemons, dedicated Git sshd and operations timers       |
| `package`                                 | package                             | `pkgs.kajiba-server`                                                          |
| `ui.enable`, `ui.package`                 | boolean, package                    | `true`, `pkgs.kajiba-ui`; serves the store bundle with `--ui-dir`             |
| `stateDir`                                | absolute string                     | `/var/lib/kajiba-server`                                                      |
| `git.stateDir`                            | absolute string                     | `services.kajiba-git.stateDir`; read-only repository access                   |
| `git.socket`                              | absolute string                     | `/run/kajiba-git/local.sock`                                                  |
| `git.groupIds`                            | read-only list of unsigned integers | derived known Git-side GIDs for secret isolation                              |
| `http.publicUrl`                          | string                              | required; external HTTPS origin                                               |
| `http.listen`                             | nullable string                     | `127.0.0.1:8080`                                                              |
| `http.reflection`                         | nullable boolean                    | `false`                                                                       |
| `ssh.host`                                | nullable string                     | host of `http.publicUrl`; advertised clone host                               |
| `ssh.port`                                | nullable port                       | dedicated `services.kajiba-git.ssh.port`; explicit `null` keeps Rust's `2222` |
| `ssh.probe`                               | nullable string                     | `127.0.0.1:<dedicated sshd port>`; `null` disables the readiness banner probe |
| `log.level`                               | nullable string                     | `info`; tracing filter                                                        |
| `metrics.listen`                          | nullable string                     | no listener; when set, loopback only                                          |
| `auth.sessionTtl`, `auth.enrollmentTtl`   | nullable strings                    | `7d`, `15m`                                                                   |
| `users`                                   | attribute set of submodules         | `{}`; declarations below                                                      |
| `runnerListener`                          | nullable submodule                  | `null`; required when runners are declared                                    |
| `runners`                                 | attribute set of submodules         | `{}`; declarations below                                                      |
| `mirror.credentials.<name>.tokenFile`     | secret-file string                  | runtime GitHub PAT file; credentials default to `{}`                          |
| `mirror.reconcileInterval`                | nullable string                     | `1h`                                                                          |
| `repos`                                   | attribute set of submodules         | `{}`; declarations below                                                      |
| `ci.logRetention`, `ci.artifactRetention` | nullable strings                    | `14d`, `14d`                                                                  |
| `ci.runAt`                                | nullable string                     | `03:00`, local `HH:MM`; in-process retention, no retention timer              |
| `ci.worktreeRetention`                    | nullable string                     | `1d`, positive duration                                                       |
| `dump.dir`                                | absolute string                     | `/var/lib/kajiba-dumps`                                                       |
| `dump.keep`                               | nullable unsigned integer           | `7`; zero still preserves the newly completed dump (0171)                     |
| `persistedPaths`                          | read-only list of strings           | server state directory, Git state directory, `dump.dir`                       |

For each `users.<name>`:

| Option                   | Type                        | Default / meaning                                                                                                                   |
| ------------------------ | --------------------------- | ----------------------------------------------------------------------------------------------------------------------------------- |
| `isAdmin`                | boolean                     | `false`                                                                                                                             |
| `sshKeys`, `signingKeys` | lists of strings            | `[]`; OpenSSH public keys, surrounding whitespace trimmed                                                                           |
| `tokens`                 | attribute set of submodules | `{}`; each entry requires `sha256` (string) and `scopes` (list of strings); `expires` is a nullable RFC 3339 string, default `null` |

`runnerListener`, when set, requires `listen`, `trustDomain`, `svidFile`,
`keyFile`, `bundleFile` (strings). Certificate and bundle paths are absolute;
`keyFile` is a secret-file string. This is the separate runner mTLS listener,
not the public HTTP origin.

For each `runners.<name>`, `spiffeId` is a required string. Its `capabilities`
requires string `arch` (`x86_64` or `aarch64`) and `os` (`linux` or `darwin`).
Nullable `backends`, `tags` and `maxJobs` keep defaults `["nix"]`, `[]` and `1`;
the module types are lists of strings and a positive integer respectively.

For each `repos.<stable-key>`:

| Option                        | Type                     | Default / meaning                                                                                |
| ----------------------------- | ------------------------ | ------------------------------------------------------------------------------------------------ |
| `owner`, `description`        | strings                  | required; owner is a declared user                                                               |
| `name`                        | nullable string          | attribute key; change this to rename, keep the stable key                                        |
| `visibility`, `defaultBranch` | nullable strings         | `public`, `main`                                                                                 |
| `ci`                          | nullable enum            | `off`; `off`, `manual`, `push`                                                                   |
| `previousNames`               | list of strings          | `[]`; former `owner/name` redirects                                                              |
| `permissions`                 | attribute set of strings | `{}`; user to `read`, `write` or `admin`                                                         |
| `mirrors`                     | list of submodules       | `[]`; each requires string `remoteUrl` and `credential`; nullable boolean `enabled` keeps `true` |

The hidden legacy `ciEnabled` declaration only reports its removal; setting it
is refused. Use `ci`.

## `services.kajiba-git`

Enabled by the server module, with no independent `enable` option.

| Option           | Type            | Effective default                                              |
| ---------------- | --------------- | -------------------------------------------------------------- |
| `package`        | package         | `pkgs.kajiba-git`                                              |
| `stateDir`       | absolute string | `/var/lib/kajiba-git`                                          |
| `ssh.port`       | port            | `2222`; dedicated public-key-only sshd, opened in the firewall |
| `log.level`      | nullable string | `info`                                                         |
| `metrics.listen` | nullable string | no listener; loopback only when set                            |

Hooks are generated from the Git package. Runtime socket directories, peer
identities and Git's server socket keep their fixed module defaults (0093,
0095). The Rust development overrides are deliberately not module options.

## `services.kajiba-runner`

| Option                            | Type                      | Effective default / meaning                                  |
| --------------------------------- | ------------------------- | ------------------------------------------------------------ |
| `enable`                          | boolean                   | `false`                                                      |
| `package`                         | package                   | `pkgs.kajiba-runner`                                         |
| `name`, `url`                     | strings                   | required; declared runner name and HTTPS runner-listener URL |
| `tls.trustDomain`, `tls.serverId` | strings                   | required; trust domain and forge SPIFFE ID                   |
| `tls.svidFile`, `tls.bundleFile`  | absolute strings          | required; runner certificate and trusted CA bundle           |
| `tls.keyFile`                     | secret-file string        | required; runner private key                                 |
| `hostname`                        | nullable string           | OS hostname, informational                                   |
| `backends`, `tags`                | nullable lists of strings | `["nix"]`, `[]`                                              |
| `maxJobs`                         | nullable positive integer | `1`                                                          |
| `stateDir`                        | absolute string           | `/var/lib/kajiba-runner`                                     |
| `log.level`                       | nullable string           | `info`                                                       |
| `metrics.listen`                  | nullable string           | no listener; loopback only when set                          |

## Persistence, secrets and unit contracts

Persist the whole server and Git state directories and `dump.dir`, as exposed by
`persistedPaths`; Git staging and quarantine must share a filesystem with its
repositories. Server state and dumps belong to `kajiba-server:kajiba-server`,
Git state to `kajiba-git:kajiba-git`, all mode 0750. The server belongs to the Git
group for read access; the Git user cannot read server state. Custom paths use
structured tmpfiles rules; paths under `/var/lib` use `StateDirectory`. Persist
runner state separately if jobs must survive loss of an ephemeral root; it is
private to `kajiba-runner:kajiba-runner`, mode 0700.

A secret-file option accepts only an absolute string outside the Nix store,
without Nix string context; never pass secret contents or a Nix path. Generated
TOML contains paths and declared PAT hashes only. Provision files for the service
user before startup. Structural validation never opens secrets; runtime startup
and `config check --files` do. Server secrets must not be group-writable,
world-accessible or readable by any Git-side group (0073, 0074). Known tmpfiles,
`environment.etc`, sops and agenix metadata is checked at evaluation; actual
runtime ownership, ACLs and parent-directory access remain deployment concerns.

`kajiba-dump.timer` runs daily, `kajiba-gc.timer` weekly, both persistent. Their
hardened oneshot clients run as the respective service users, use only Unix
sockets, have no writable state paths and wait for the admitted operation to
complete. The server owns dump creation and retention; the Git daemon owns gc.
No automatic quarantine deletion or separate retention timer is installed.
External-copy coordination and the full restore procedure are documented by
the J8 backup handoff task, not by additional module options.

The server reads `/etc/kajiba/server.toml`. Changes to `users`, `runners`, `repos`
and `mirror` send SIGHUP without restarting. All other settings, the server
package and the enabled UI bundle cause a restart; a restart wins over a reload.
Manual SIGHUP applies declarations but retains restart-only values and logs
`config-reload-partial`. Invalid reloads preserve the running configuration.
Git has no reload; Git config/package changes restart it. Runner config/package
changes restart the runner.

HTTP, optional runner mTLS and both Git sockets belong to systemd and remain
open across ordinary daemon restarts (0157). A listener address change replaces
its socket. Both daemons allow five starts in five minutes; exhausting the
budget fails their activating sockets too (0160). Repair the cause, reset failed
service/socket units, then start the service. Git drains for at most 30 seconds,
with `KillMode=mixed` and a 35-second unit stop bound.

Linux runner jobs use independent transient units. The private mode-0600
`kajiba-job.socket` admits only fixed start/stop operations with canonical attempt
IDs to a root broker, without arbitrary commands or unit properties. The broker
and jobs do not depend on the runner service. Jobs keep the untrusted runner
identity and sandbox and survive runner restarts; no polkit or sudo authority
is granted (0156).

## Verification

`checks.<system>.module-eval` pins generated TOML, defaults, persistence,
secret isolation, timer hardening, runner broker isolation and reload/restart
wiring. The accompanying `module-*` checks prove invalid generated values fail
build-time validation. `checks.<linux system>.nixos-server-config` covers runtime
secret validation and activation failure/recovery;
`checks.<linux system>.nixos-server-lifecycle` covers declaration and partial
reloads, socket continuity and bounded restarts. Runner job survival remains
covered by `checks.<linux system>.nixos-ci-recovery`.
