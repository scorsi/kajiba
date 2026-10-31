# ADR 0126 — Runner listener bounds

Status: Accepted — 2026-10-31. Amends 0119.

## Context

`kajiba.runner.v1` moves from the public port to a TLS listener of its own (0124). ADR 0119 bounds what a caller costs before admission on the public port, which caps its connections at 1024 and reads headers within 30 s. A TLS listener adds a cost before any request: the handshake, which an unauthenticated caller can start and leave unfinished. The owner decided the listener's bounds on 2026-10-31.

## Decision

- **Amends 0119 — the runner listener's own bounds.** Product constants, not configuration:

  | Point         | Value                                                                    |
  | ------------- | ------------------------------------------------------------------------ |
  | Connections   | at most 64 at once, from acceptance (handshake included) to closing      |
  | TLS handshake | completed within 10 s, else the connection is closed                     |
  | Protocol      | HTTP/2 only, negotiated by ALPN `h2`; HTTP/2 keep-alive pings every 30 s |

- The bounds of 0119 on admission (16 waiting requests, 10 s each), message size and bundle transfers apply unchanged, on the runner listener instead of the public port.
- A connection beyond the 64 waits in the listener's backlog until one ends; a handshake that fails or times out closes the connection without a response, a log line or a refusal count (0124).

## Consequences

- Unauthenticated callers hold at most 64 connections for at most 10 s each before verification; they can delay runners, not exhaust the forge, and never reach the public port's connections.
- 64 connections cover far more runners than a single forge is expected to declare: each runner keeps one connection, its bundle fetches multiplexed on it.

## Rejected alternatives

- Sharing the public port's 1024 connections: a flood of handshakes would then starve the web UI and Git over HTTP.
- No handshake deadline: an unfinished handshake would hold a connection for as long as TCP keeps it.
