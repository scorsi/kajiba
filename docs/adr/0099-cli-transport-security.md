# ADR 0099 — CLI tokens only over HTTPS

Status: Accepted — 2026-10-30. Amends 0089.

## Context

ADR 0089 has the CLI use TLS for an `https` forge URL, and leaves an `http` URL unrestricted. A token sent over plain HTTP to another machine, including the `kajiba login` check, can be read and replayed by anyone on the path. The owner decided on 2026-10-30 to refuse it.

## Decision

- **Amends 0089 — transport.** The CLI sends a personal access token, in every command including `login`'s check call, only to an `https` URL, or to an `http` URL whose host is `localhost`, `127.0.0.1` or `[::1]` (any port), for development against a local server.
- Any other URL is refused before a connection is opened, with an error naming the URL; `login` then writes nothing.

## Consequences

- A forge reached over the network needs TLS: the reverse proxy in front of the plaintext `http.listen` provides it (0005, 0019).
- A development server on another machine is reached through a tunnel to a loopback port, or over HTTPS.

## Rejected alternatives

- A flag allowing plain HTTP: one more way to leak a token, for a case a loopback tunnel already covers.
- Treating every private address as local: a shared network still exposes the token.
