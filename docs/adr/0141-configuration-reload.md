# ADR 0141 — Configuration reload on `SIGHUP`

Status: Accepted — 2026-10-31. Amends 0019, 0022, 0026, 0107.

## Context

ADR 0019 reloads the configuration by restarting the service. Since 0026 every repository, permission, mirror, runner, user, key and declared token is configuration, so adding a repository restarts `kajiba-server` and interrupts every session, stream and push in progress. The owner decided on 2026-10-31 that declarations reload without a restart. J8 planned the reload; this ADR brings it forward for the server.

## Decision

- **Amends 0019 — signal.** On `SIGHUP`, `kajiba-server` reads its `--config` file again and validates it as at startup (structure and files). An invalid file changes nothing: the server keeps its running configuration and logs `config-reload-failed{err}` (error).
- **Reloadable keys**: `users` (with their SSH and signing keys and declared tokens), `repos` (permissions, mirrors, CI mode and previous names included), `runners` and `mirror`. They are applied by the startup reconciliation (0014, 0026, 0107), in its single transaction, under the writer lock: requests that write wait for it. A refusal (a name held by an archived repository, the git side unavailable for a creation) commits nothing and logs `config-reload-failed{err}`. Creation, rename, archival and unarchival keep their 0026 semantics.
- **What a reload does not interrupt**: sessions, open streams and in-flight jobs, except what reconciliation itself defines (a removed user's credentials, the runs of an archived repository, 0026). A connected runner whose declaration is removed or whose `spiffe_id` changes has its `Connect` stream ended with `UNAVAILABLE`; its reconnection is admitted or refused against the new declaration (0124). Other runners keep their stream; changed capabilities apply at their next connection. Only startup marks every runner disconnected (amends 0107).
- **Keys that need a restart**: every other key — `state_dir`, `run_dir`, `http`, `git`, `ssh`, `log`, `metrics`, `auth`, `runner_listener`, `ci`, `dump` — and the `--ui-dir` argument. When some of them differ, the reloadable keys are applied, the others keep their running values, and `config-reload-partial{generation,keys}` (warn) names them.
- **Generation.** The configuration generation is 1 at startup and grows by one per applied reload, partial included. Each applied reload logs `config-reloaded{generation}`; the metric `kajiba_config_generation` (gauge) exposes it.
- **Concurrency.** Reloads run one at a time; `SIGHUP`s received during a reload are coalesced into one more reload.
- **Module.** The unit reads the stable path `/etc/kajiba/server.toml`, so a configuration change alone leaves its command line unchanged. `ExecReload` sends `SIGHUP` to the main process. The generated file is the unit's reload trigger; its restart triggers are the package and the settings outside the reloadable keys. A NixOS switch therefore reloads the server when only declarations change and restarts it when the package, the UI bundle or a restart key changes (a restart wins when both change). `systemctl reload` returns once the signal is sent; the log line and the generation show when the reload is applied.
- **kajiba-git has no reload.** Its configuration (state directory, hooks path, log, metrics) is all restart keys, and repositories are created by the server's reconciliation through the git side protocol (0053).

## Consequences

- Creating, renaming or archiving a repository, and adding a user, key, token, runner or mirror, no longer restart the server (0026's "Restarting the server applies them" now reads "reloading").
- 0022 gains `config-reloaded{generation}`, `config-reload-partial{generation,keys}` (warn), `config-reload-failed{err}` (error) and `kajiba_config_generation`.
- The generation is available to a later admin page.

## Rejected alternatives

- Two generated files, one per part: the server would read two configuration paths and 0019's single file would split; computing the restart triggers from the settings keeps one file.
- Reloading every key: rebinding listeners or moving state directories under live requests buys little over a prompt restart (0140).
- A reload command on the admin socket: `SIGHUP` is what `systemctl reload` and NixOS reload triggers drive.
