# ADR 0154 — Activity ranges change granularity

Status: Accepted — 2026-11-01. Amends 0134, 0145, 0149.

## Context

The owner's J5.1-a A7 decision replaces the 30/90-day daily ribbons with
meaningful ranges, using existing APIs and the viewer's local calendar.

## Decision

- Activity and repository Pulse share the same chart and three ranges:
  **7 days** (seven day rows by 24 local-hour columns), **30 days** (daily
  bars), **1 year** (one contribution cell per Monday-based calendar week,
  ordered oldest first, down seven rows then across). Default is 30 days;
  `days=7|30|365` selects the range. Unsupported values, including old 90-day
  links, fall back to 30 days. There is no one-day range.
- Days, hours and weeks use the viewer's local calendar, never 24-hour steps.
  The year starts the day after the same calendar date last year and ends
  today (February 29 clamps to February 28 in the prior year); boundary weeks contain only in-range events. A leap-year interval
  can contain 366 days. Keys include the local date (and hour or week prefix).
  To keep exactly 24 hour columns, repeated autumn hours share a cell and
  skipped spring hours are empty. The chart explains this.
- Heatmap intensity and visible counts are grouped pushes plus runs. Failed
  runs add both a red edge and `!`. Every bucket is a keyboard-operable button
  with detailed text counts, including zero buckets. Native hover tooltips
  carry the same breakdown as the accessible label.
- Daily runs stack succeeded, failed, cancelled, running and queued outcomes.
  Worktree runs have a separate, lighter stack. A thin companion bar counts
  grouped pushes and a small number gives their commit count; diamonds mark
  tags. Tooltips break down commits, runs and outcomes by repository in
  Activity, by ref in Pulse (unreferenced manual and worktree runs labelled
  separately). Median and longest durations use finished runs only, including
  worktrees; median averages the middle two values for even counts.
- The trend is the rolling seven-calendar-day branch/manual success rate:
  succeeded / (succeeded + failed). Worktree, cancelled and active runs do not
  contribute. Empty windows leave gaps. The six days before the first bar
  contribute when loaded.
- Clicking selects a bucket in `day` and lists all its loaded grouped pushes,
  commits, runs and tags in the internally scrolling 12-rem list. Repository
  selection also travels to the first listed commit when present, preserving
  the existing separate `at` and history scope. Commit navigation preserves
  the bucket; changing range clears it.
- No new API or dependency. Activity retains default-branch commits and all
  loaded runs; Pulse retains selected-ref/path commits and repository runs
  (path scope restricts runs to the listed commits). No extra history calls
  are made for the year or on range switches. Existing caps remain: Activity
  requests 300 commits (up to 400 with 200-entry pages) and 50 runs per repo;
  Pulse requests 400 scoped commits and 60 runs. Commit paging stops after
  eight requests even for short path-filtered pages. The chart always labels
  this as a loaded sample, which can omit older activity.
- There are no push timestamps or tag creation times in these APIs. A chart
  push means commits grouped by repository, author identity and local hour,
  dated by commit time; tags use their target commit's time. These limits are
  visible beside the chart. History's author/day groups remain as in 0149.

## Consequences

The year is a bounded sample, not a complete activity audit. Runs are counted
by creation time, independently of whether their commit appears in the loaded
history. Worktree outcomes do not affect the branch success trend. Existing
permissions and server contracts are unchanged. Node tests cover local hours,
calendar days/weeks through both Europe/Paris DST transitions, boundaries,
aggregates and success windows; Playwright covers both range selectors and
bucket lists. No NixOS test changes.
