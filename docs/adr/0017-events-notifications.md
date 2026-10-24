# ADR 0017 — Internal events, notifications, webhooks

Status: Accepted — 2026-10-09 (revised 2026-10-24: envelope and checkpoints accepted)

## Context

Push → CI, push → mirror, run finished → information. No identified need for outgoing webhooks.

## Decision

- **Internal event bus**: every event is first written to `event` (0013) **in the same transaction as the DB change it reports** (Git ref updates use the durable `pending_push` conversion of 0003), then dispatched to registered handlers (`ci`, `mirror`, later `notifier`, `webhook`). Handlers are idempotent and replayable from the journal.
- **Envelope**: `id` (sequence), `public_id`, `kind` (kebab-case: `push`, `run-finished`, `mirror-synced`, `auth`…), `version` (payload schema version), `occurred_at`, principal (`user` | `runner` | `system` + id), `repo_id`, `correlation_id` (the request id, 0022), `payload` (JSON object). One `push` event per updated ref.
- **Handlers and checkpoints**: each handler has a stable name and a row in `handler_checkpoint`. It processes events in `id` order; for each event, **its DB effect and the checkpoint advance commit in one transaction**, and effects carry a unique event-derived key (`run.event_id`, the mirror's coalesced row) so a replay cannot duplicate them. A handler whose transaction fails keeps its checkpoint and retries with capped backoff; the failure is logged and counted (0022). Handlers with external effects (notifier, webhook, later) only enqueue a DB job in that transaction; the job performs the call, at least once. Ordering by `id` is safe because the server is the single writer (0012).
- Dispatch is woken in-process after each commit; at startup each handler resumes from its checkpoint.
- v0: mirror and run status visible in the UI and the CLI; no external notification.
- Later: `Notifier` interface (ntfy as a first implementation); outgoing webhooks = one more handler, no model change.

## Consequences

- Event replay (after an outage or a redeploy) is a first-class, tested mechanism.
- A handler stuck on one event blocks only that handler; it is visible in metrics and logs.

## Rejected alternatives

- Webhooks from v0: no consumer.
- Direct calls between modules (push → mirror function): prevents replay and adding handlers.
- An in-memory queue in front of the journal: loses events on crash.
