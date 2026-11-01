# ADR 0162 — Wait for the Git session snapshot before admission

Status: Accepted — 2026-11-01. Amends 0053, 0061, 0157, 0161.

## Context

A handoff queued during Git drain can reach the new generation after its Hello
but before the server rebuilds the admission gate from its session snapshot.
Refusing that handoff immediately breaks the queued push across a valid restart.

## Decision

- OpenSession waits up to three seconds for the admission gate's snapshot to
  match the generation recorded by Hello. It releases the writer lock between
  checks so snapshot rebuilding and session cancellation can proceed.
- Admission rechecks the generation under the writer lock on every check and
  retains the existing authorization, closed-session and identity checks.
  A missing Hello or a changed generation still fails immediately once the
  writer lock is acquired.
- If the bound expires, including while waiting for the writer lock, it returns
  the existing UNAVAILABLE / GIT_SIDE_UNAVAILABLE: session snapshot required
  error without registering a session or starting a child.
- No protocol field, configuration option or dependency is added. The three-second
  admission wait fits inside the existing five-second helper reply bound;
  Git's thirty-second drain remains unchanged.

## Consequences

Sessions queued across a restart can proceed after snapshot recovery instead of
losing a startup race. A missing snapshot still fails closed within a fixed bound.
Unit tests cover waiting followed by successful admission, timeout without a live
session, and a successful retry after snapshot recovery. The existing
nixos-server-lifecycle queued signed push remains the integration check.
