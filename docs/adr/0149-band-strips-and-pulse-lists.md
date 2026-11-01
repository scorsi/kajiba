# ADR 0149 — Band strips and bounded Pulse lists

Status: Accepted — 2026-11-01. Amends 0134, 0145, 0147.

## Context

The owner's J5.1-a feedback requests full-width folding strips, a stable Pulse
header, bounded day lists, and both commit and push counts in History.

## Decision

- Every foldable band uses the shared full-width `BandStrip` link in both
  states. Pulse uses the same fixed-height header above its content, including
  on account, activity, administration and run pages. `pulse=0` folds it;
  absence opens it, preserving the other query state without resetting scroll.
- Repository Pulse's commit list has a maximum height of 12 rem and scrolls
  internally. A day selects its list with `day`; selecting a commit changes
  `at` independently, preserving the list and its position (0147).
- History retains commit totals on its Commits filter and also shows the number
  of commit groups as pushes, explicitly labelled as grouped by author/day.
  These are the existing browser-composed groups (0145), not server push events;
  no push-event API is introduced.
- Repository pipeline cards wrap their state/id/duration row and truncate long
  metadata and step labels, with complete text available on hover.

## Consequences

No API, dependency, permission or NixOS test changes. The browser e2e suite
covers strip-edge clicks, stable selection and a day list stressed with extra
rows to verify internal scrolling independently of the small repository fixture.
