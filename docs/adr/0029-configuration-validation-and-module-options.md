# ADR 0029 — Configuration validation and module options

Status: Accepted — 2026-10-24

## Context

J1 implemented the configuration file, `config check` and the server module (0015, 0019). Implementation and the J1 security review showed points of validation and of the module's option surface that ADRs 0015 and 0019 left implicit. As README says, an accepted ADR is not edited: each item below names the ADR point it amends; everything else in that ADR stands. Each item describes what the J1 code does and is covered by its tests.

## Decision

- **Amends 0019 — duplicate credentials.** `config check` refuses an SSH key fingerprint declared by two users for the same usage (authentication or signing; a user listing its own key twice is accepted) and a token hash declared twice, naming both fields, instead of failing on the database's unique constraints at startup. Token hashes must be exactly 64 lowercase hexadecimal characters.
- **Amends 0019 — TTL bounds.** `config check` accepts `auth.enrollment_ttl` from 1 minute to 24 hours and `auth.session_ttl` from 1 minute to 90 days, and refuses any other value, naming the field. Expiries are computed with a checked conversion to milliseconds and a checked addition. A value that cannot be represented is an error and nothing is written. It never wraps around.
- **Amends 0015 and 0019 — the server module's options.**
  - The module has typed options only and no raw `settings` escape hatch: an unknown option is an evaluation error. Secrets are only absolute file paths (strings, never Nix paths copied to the store); token entries are SHA-256 hashes.
  - Scopes and durations are plain strings in the module; `kajiba-server config check`, run at build time on the generated file, is their single validator. The generated file is exposed at `/etc/kajiba/server.toml`.

## Consequences

- A configuration with a duplicate credential or a TTL outside its range fails at build time (`config check` on the generated file), not at runtime.
- Module and configuration type never disagree on what a valid scope or duration is: there is one validator.

## Rejected alternatives

- A raw `settings` option in the module: unknown keys would only fail at build time, and secrets could be written inline.
- Unbounded TTLs with saturating arithmetic: a typo such as `90000d` would silently mean "forever".
