# ADR 0104 — Plain HTTP only in an explicit development mode

Status: Accepted — 2026-10-30. Amends 0099.

## Context

ADR 0099 has the CLI send a token over `https`, or over plain `http` to `localhost`, `127.0.0.1` or `[::1]` for development against a local server. Loopback `http` is accepted silently, in every invocation. J6 adds a second client sending a credential, the runner (0008). The owner decided on 2026-10-30 that plain `http` is a development choice that must be asked for, and that every client follows the same rule.

## Decision

- **Amends 0099 — loopback `http` no longer accepted by default.** A client sends a credential only to an `https` URL. Plain `http` is accepted only when both hold: the host is `localhost`, `127.0.0.1` or `[::1]` (any port), and the client runs in an explicit development mode, off by default. Any other URL is refused before a connection is opened, with an error naming the URL; `kajiba login` then writes nothing.
- One rule for every client, in `kajiba-core`. The development mode is:
  - for the CLI, the global `--development` flag;
  - for the runner, a configuration key (J6);
  - for `pnpm dev`'s proxy (0102), always on: it is a development tool.
- A client using plain `http` in development mode says so once per run: a warning on stderr for the CLI and `pnpm dev`, a warning log line at start for the runner.

## Consequences

- A local development server is reached with `kajiba --development`; scripts and tests against a loopback forge pass the flag.
- A production configuration cannot fall back to plain `http` by naming a loopback URL by mistake: the flag or key shows in the command or the configuration.

## Rejected alternatives

- Keeping silent loopback `http` (0099): a credential in clear text without anyone having asked for it, and a different rule for the runner.
- An environment variable for the CLI: invisible in the command line that sends the token.
