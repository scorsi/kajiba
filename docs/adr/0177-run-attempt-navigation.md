# ADR 0177 — Run attempt navigation

Status: Accepted — 2026-11-01. Amends 0142 and 0152.

## Context

Owner feedback asks for the latest attempt on opening a run, prompt feedback
when changing attempts, and stable scrolling while reading earlier output.

## Decision

- Open the latest attempt by default. A valid `#L<attempt>-<line>` selects its
  attempt before `?step`; an explicit step still selects that attempt otherwise.
- Keep run metadata on attempt changes. Read only the selected stored section
  through `api.ts` when switching, starting at its existing WatchRun section
  offset and closing at the next section. Abort the previous read on selection changes or leaving.
  Show a log skeleton and mark the selected attempt busy while loading; expose
  a retry on failure. Keep the existing single live run stream independent of
  selection; a temporary stored-section read may coexist with it. Reuse the initial loaded log on opening.
- Land on an explicit line, otherwise the first error for completed logs or
  the end for live logs, once per selection. Subsequent output follows only
  within four pixels of the bottom. Disable browser scroll anchoring in the
  console and show “Jump to latest” when output arrives while scrolled up.
  Completion and metadata refreshes preserve the reader's position.
- Show only the declared runner name; retain the hostname in its tooltip.

## Consequences

No protocol or dependency changes. Initial page loading still discovers attempts
through the stored log stream; switching reads no other run metadata.
Node tests cover selection and section reads; Playwright covers selection,
loading, scrolling and run navigation.
