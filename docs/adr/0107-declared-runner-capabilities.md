# ADR 0107 — Declared runner capabilities

Status: Accepted — 2026-10-30. Amends 0019, 0026.

## Context

ADR 0026 declares runners in the server configuration, `runners.<name>.capabilities` being what a runner may announce (0008), but gives the field no default, no value syntax and no rule for an absent declaration. It also leaves open how reconciliation writes the `runner` row, whose runtime columns (version, connection) come from the runner itself. The owner decided on 2026-10-30 that a declared runner's required fields are its name, its secret and its capabilities, and that the runner reports its host name at connection, recorded for display only.

## Decision

- **Amends 0026 — capabilities are required.** A declared runner has three required fields: its name (the table key), `token_file` and `capabilities`. A declaration without capabilities is refused; there is no default system.
- **Amends 0019 — capability fields.**

  | Field                                  | Default   | Meaning                                                              |
  | -------------------------------------- | --------- | -------------------------------------------------------------------- |
  | `runners.<name>.capabilities.arch`     | required  | the CPU half of the runner's Nix system name: `x86_64` or `aarch64`  |
  | `runners.<name>.capabilities.os`       | required  | the kernel half of the runner's Nix system name: `linux` or `darwin` |
  | `runners.<name>.capabilities.backends` | `["nix"]` | backends the runner may announce (0009)                              |
  | `runners.<name>.capabilities.tags`     | `[]`      | tags the runner may announce (0008)                                  |
  | `runners.<name>.capabilities.max_jobs` | `1`       | concurrency the runner may announce, greater than zero               |

  The defaults are the runner's own (0019), so a runner configured with defaults matches a declaration with defaults. `arch` and `os` use Nix names, not a toolchain's (`darwin`, never `macos`). A tag is 1 to 63 characters, lowercase ASCII letters, digits, `.`, `_` or `-`, starting with a letter or a digit, and is matched exactly, never normalized. `config check` refuses each violation naming the field (`runners.<name>.capabilities.os`, `runners.<name>.capabilities.tags[<index>]`).

- **Amends 0019 — server example.** Its runner declaration becomes:

  ```toml
  [runners.build-1]
  token_file = "/run/secrets/kajiba-runner-build-1"

  [runners.build-1.capabilities]
  arch = "aarch64"
  os = "linux"
  backends = ["nix"]
  tags = ["kvm"]
  max_jobs = 2
  ```

- **Reconciliation.** In the declared pass at startup, after repositories, `runner` rows are matched by name: a new name gets a row; changed capabilities update the row, whose identity and runtime columns stay; a name no longer declared gets `disabled_at` (the row stays, its attempts reference it, and its connections are refused, 0026); a re-declared name clears `disabled_at` on the same row. The capabilities are stored as opaque text (0013). No connection survives a restart: every row starts disconnected. `version` stays empty until the runner's first connection.
- **Host name.** `runner.hostname` holds the host name a runner reports at connection. It is shown, never used for authorization or matching.

## Consequences

- Adding a runner is a reviewed deployment change that names the system it builds for; a host announcing another system is refused (0026).
- A configuration error that drops a runner loses no history; re-declaring it restores the same row.

## Rejected alternatives

- Defaulting `arch` and `os` to the server's own system: a declaration would silently mean something else on another forge host.
- Free-form `arch` and `os` strings: a typo (`macos`, `arm64`) would only show up as a runner never matching any job.
- Case-insensitive tags: Nix system features, which tags will carry for worktree runs (0097), are case-sensitive.
