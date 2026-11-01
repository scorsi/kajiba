# ADR 0152 — Run page console layout

Status: Accepted — 2026-11-01. Amends 0142.

## Context

The owner’s J5.1-a feedback asks for less scrolling before the log and more room to read it.

## Decision

“What changed” is folded by default for every run state; `changed=1` opens it. Steps fill the viewport space remaining below the open bands, with a minimum height of 16 rem when those bands leave less space. Log panes scroll internally. The log number gutter reserves six monospace digits plus padding at every screen width.

## Consequences

Fold state remains in the URL. Opening more bands or viewing a short screen may require scrolling the outer run page to reach the console; log content still scrolls inside its pane. Log transport and decoding remain unchanged.
