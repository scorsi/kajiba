# ADR 0181 — Restore daily Pulse ribbons

Status: Accepted — 2026-11-01. Amends 0149, 0154.

## Context

The owner finds the new seven-day, monthly and yearly Pulse charts unreadable
and prefers the design before A7. Pulse should have a lighter visual identity
than the other bands, without a redundant page-title summary.

## Decision

Restore the pre-A7 daily commit bars: Activity defaults to 90 days with its
30/90-day selector, release markers and CI outcome lane. Repository Pulse
shows 90 daily bars scoped to the selected ref/path, release markers, and
its recent commits or selected day's commits. Unsupported Activity `days`
values fall back to 90 days; repository Pulse ignores that parameter.
Remove the unused A7 charts, aggregation helpers and their tests.

Keep local calendar day arithmetic through DST, unique list keys, the bounded
12-rem repository day list, independent `day` and `at` selection, and aligned
duration bars. Existing API loading bounds and permissions remain unchanged.

Pulse headers retain full-width fold links and URL state but use a surface
background without the horizontal border of other strips. Their summaries
appear only when folded. Activity and repository Pulse contents also omit
the full-width bottom border. Repository descriptions and scope captions
remain useful context.

## Consequences

No API, dependency or NixOS changes. Node regressions retain calendar and
key coverage; Playwright covers restored daily bars, period selection,
keyboard day selection, preserved query state, bounded lists and lighter
Pulse headers. Accepted ADR 0154 remains the superseded A7 design record.
