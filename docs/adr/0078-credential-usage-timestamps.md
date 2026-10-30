# ADR 0078 — Credential usage timestamps

Status: Accepted — 2026-10-30

## Context

`api_token.last_used_at` and `session.last_seen_at` (docs/schema.md) record when a credential was last used. The auth interceptor (0014, 0020) resolves a credential on every `kajiba.v1` call, and every write goes through the single writer (0012). A write per call would make every authenticated read contend for that writer.

## Decision

- **Amends 0014 — usage writes.** The interceptor writes `api_token.last_used_at` or `session.last_seen_at` at most once per minute per token or session, through the single writer: only when the stored value is absent or at least 60 seconds old, with the same condition in the `UPDATE` so concurrent calls write once.
- The timestamp is informational: it never extends a session, whose expiry stays absolute (0014).

## Consequences

- The recorded usage time can lag the real last use by up to one minute.
- A burst of authenticated reads costs at most one write per credential and minute.

## Rejected alternatives

- A write on every call: contention on the single writer for read-only traffic.
- An in-memory throttle: state to keep for nothing the conditional `UPDATE` does not already bound.
