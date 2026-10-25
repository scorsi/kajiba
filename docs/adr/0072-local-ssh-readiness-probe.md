# ADR 0072 — Local SSH readiness probe

Status: Accepted — 2026-10-25

## Context

J3 review quality note 15 identifies the undocumented `ssh.probe` server
setting. ADR 0054 describes `ssh.host` and `ssh.port` as display settings;
ADRs 0022 and 0052 require readiness to check the SSH integration.

## Decision

- **Amends 0054 — server configuration.** `ssh.probe` is an optional socket
  address for the local dedicated sshd. It is independent of the host and
  port advertised in clone URLs. Standalone TOML defaults to no probe:
  omitting the field disables only the sshd readiness check.
- The NixOS option is `services.kajiba-server.ssh.probe`, a nullable string.
  Its default is `127.0.0.1:<services.kajiba-git.ssh.port>` (normally 2222),
  so the module enables the check for its dedicated sshd. Setting it to
  `null` omits the TOML field and disables that check.
- `config check` requires an IP socket address with a loopback IP and a
  nonzero port. Hostnames, non-loopback addresses and port zero are refused.
  The module's generated-file validation uses the same rule.
- **Amends 0022 and 0052 — SSH readiness.** With a probe configured, each
  readiness evaluation has a single one-second timeout covering TCP connect
  and reading the first eight bytes. Those bytes must be `SSH-2.0-`.
  Connection failure, timeout, short input or another prefix means not ready.
  With the probe disabled, the other readiness checks still apply.
- A probe result, successful or failed, is shared for a constant **three
  seconds** by `/readyz` and the health RPC; concurrent misses are coalesced, and
  a probe that is cancelled counts as failed for that period. This bounds the
  work anonymous readiness requests can send to the sshd.
- The check establishes only a reachable endpoint emitting an SSH v2 banner
  prefix. It does not authenticate the sshd, exercise key lookup or a forced
  command, or prove a clone or push will succeed. Those remain integration
  test responsibilities.

## Consequences

- Standalone tests and installations can disable the probe; module deployments
  check the local dedicated listener without using an advertised public host.
- Rust tests cover defaults, validation, banner failures, recovery and disabling.
  Existing module checks pin defaults, custom ports and `null` omission.

## Rejected alternatives

- Probing the advertised SSH host: couples local readiness to external routing.
- Treating TCP connect alone as readiness: accepts a non-SSH listener.
