# ADR 0150 — Shell navigation and login

Status: Accepted — 2026-11-01. Amends 0144.

## Context

Owner feedback on the v2 interface asks for a larger overview toggle, visible
current navigation, no runner summary in the shell, pointer cursors on controls,
and a centered sign-in card with clearer enrollment wording. A horizontal shell
redesign is deferred.

## Decision

- Keep the foldable overview and its URL state (`drawer=1`). The header's
  brand and location zone is one keyboard-accessible button, with a visible
  Open/Close overview label; account, sign-out and theme controls stay independent.
  Toggling preserves the current page's query parameters.
- Mark the current navigation entry with `aria-current="page"` and an accent
  background. Repository pages, including nested routes, select their repository
  row. Account and Administration include their nested routes. Run pages select
  Activity, which holds the forge-wide Pipelines band; there is no separate
  Pipelines page in the shell.
- **Amends 0144 — shell runner summary.** Remove the shell's runner section and
  its administration RPC call. Runners remain available in Administration;
  Activity's administrator-only operational information is unchanged.
- Enabled buttons, links and other clickable controls use the pointer cursor
  globally.
- Center the sign-in card horizontally and vertically below the signed-out
  header. Explain that accounts are created through an administrator's enrollment
  link, without a “No account yet” heading. Authentication and the return URL
  behavior are unchanged.

## Consequences

No protocol, configuration or dependency changes. Browser tests cover the toggle,
current navigation and login layout; unit tests cover nested navigation matching.
