# ADR 0110 — Runner configuration: `hostname` and `development`

Status: Accepted — 2026-10-31. Amends 0019, 0104.

## Context

ADR 0019's runner fields name the runner, its forge and its secret, but not the host name the runner reports when it connects (0105: shown by the server, never used for authorization). The owner decided on 2026-10-30 that the runner reads it from the OS by default and that the configuration can override it, for a runner whose OS name says little (a VM showing its host). ADR 0104 gives the runner a development mode as "a configuration key" without naming it.

## Decision

- **Amends 0019 — two runner fields.**

  | Field         | Default         | Meaning                                                                                                                               |
  | ------------- | --------------- | ------------------------------------------------------------------------------------------------------------------------------------- |
  | `hostname`    | the OS hostname | reported in the hello, recorded and shown by the server, never used for authorization; 1 to 253 characters without control characters |
  | `development` | `false`         | ADR 0104's development mode: `url` may then be plain `http` to `localhost`, `127.0.0.1` or `[::1]`                                    |

- **Amends 0104 — the rule is checked with the configuration.** `config check` refuses a runner `url` that the credential rule of 0104 refuses for the configured `development` value, naming `url`; at startup the runner logs `development-mode{url}` (warn) when it uses plain `http`.

## Consequences

- The complete runner example of 0019 stays valid unchanged: both fields have defaults.
- A production runner configuration cannot use plain `http`: the key shows in the file.

## Rejected alternatives

- Hostname always from the OS: a VM or container runner would show a name meaningless to the operator.
- A command-line flag for the development mode: the runner's configuration is its only interface (0019).
