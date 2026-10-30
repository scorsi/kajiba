# ADR 0093 — Configurable socket directories

Status: Accepted — 2026-10-30

## Context

ADR 0003 puts the server's admin socket at `/run/kajiba/local.sock`, and 0053 fixes the server's gate socket at `/run/kajiba/git.sock` and the git side's sockets at `/run/kajiba-git/local.sock` and `/run/kajiba-git/ssh.sock`. Only the paths each daemon dials (`git.socket` in the server's file, `server_socket` in the git side's) are configurable. The e2e harness (0015) runs both daemons unprivileged in a temporary directory, where `/run` is not writable. The owner decided on 2026-10-30 that the socket paths become configuration options with unchanged defaults.

## Decision

- **Amends 0003 and 0053 — socket directories.**
  - The server's file gains `run_dir` (default `/run/kajiba`): the server binds `<run_dir>/local.sock` (admin) and `<run_dir>/git.sock` (gate).
  - The git side's file gains `run_dir` (default `/run/kajiba-git`): the daemon binds `<run_dir>/local.sock` and `<run_dir>/ssh.sock`.
  - Both must be absolute paths. File names, modes and peer checks are unchanged.
  - `kajiba-server admin` takes `--socket <path>` (default `/run/kajiba/local.sock`) for a server run with another `run_dir`.
- The helpers that sshd and git run without a configuration (`kajiba-git ssh-keys`, `hook`, `serve-ssh`) keep dialing the default paths: a git side with another `run_dir` serves the server, not SSH pushes or hooks.
- The NixOS module keeps the defaults; it does not expose the option.

## Consequences

- A test or development run needs no root and no `/run` directory.
- Socket paths are limited to the platform's `sun_path` length: the harness uses a short temporary directory.

## Rejected alternatives

- One option per socket path: four options for what is one choice per daemon.
- An environment variable: configuration lives in the file (0019).
