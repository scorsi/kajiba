# ADR 0017 — Internal events, notifications, webhooks

Status: Accepted — 2026-10-09

## Context

Push → CI, push → mirror, run finished → information. No identified need for outgoing webhooks.

## Decision

- **Internal event bus**: every event is first written to `event` (0013), then dispatched to registered handlers (`ci`, `mirror`, later `notifier`, `webhook`). Handlers are idempotent and replayable from the journal.
- v0: mirror and run status visible in the UI and the CLI; no external notification.
- Later: `Notifier` interface with ntfy as the first implementation; outgoing webhooks = one more handler, no model change.

## Consequences

- Event replay (after an outage or a redeploy) is a first-class, tested mechanism.

## Rejected alternatives

- Webhooks from v0: no consumer.
- Direct calls between modules (push → mirror function): prevents replay and adding handlers.
