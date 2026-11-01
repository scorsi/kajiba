# ADR 0184 — Virtual run log rendering

Status: Accepted — 2026-11-01. Amends 0142.

## Context

ADR 0121 bounds stored output to 64 MiB per attempt plus a truncation marker.
Rendering every decoded line can nevertheless exhaust a viewer's DOM, especially
when output consists of many short lines.

## Decision

- Keep all received output accessible; do not introduce a browser byte or line
  cutoff. The server's per-attempt bound and truncation marker remain unchanged.
- Render only the visible lines plus eight lines of overscan on either side.
  Implement windowing without a dependency. Keep decoded text once, with compact
  numeric window indices and pixel offsets rather than off-screen DOM nodes or
  per-line presentation wrappers. Group metadata stays in the step columns.
- Measure rendered rows so long lines continue to wrap. Estimate unvisited rows
  at one line high; refine offsets as rows are visited. Compress scroll coordinates
  above eight million pixels to stay below browser scroll-height limits.
- Resolve `#L<attempt>-<n>` and first-error landing against the full line list
  before rendering the target. Search and error filtering inspect all lines and
  preserve original numbering. Live tail following, a reader's scroll position,
  and Jump to latest continue to use the console's scroll pane.

## Consequences

Browser text storage still grows with the server-bounded output of each attempt;
virtualization bounds DOM cost, not total JavaScript heap size. Native browser
find sees mounted text only; the log's text filter searches the full list.
Node tests cover window boundaries, measured heights, filtered offsets and
compressed scrolling. Playwright covers a 50,000-line attempt, deep anchors,
scrolling and live updates. No protocol, server or dependency change.
