# ADR 0086 — Application routes under `/-/`

Status: Accepted — 2026-10-30

## Context

ADR 0021 puts every UI page under `/<owner>/<name>`, with runs at `/<owner>/<name>/runs/<id>`, and avoids conflicts with other top-level routes by a list of reserved names. The web application also needs pages that belong to no repository: login, the enrollment page of the `admin enroll` link (0084), and later the account page and run pages reached from a run id alone. Adding each of them to the reserved names would take a new name away from users every time a page is added. The owner decided on 2026-10-30.

## Decision

- **Amends 0021 — application prefix.** Pages that do not belong to one repository live under `/-/`: `/-/login`, `/-/enroll#<token>` (0084), `/-/runs/<id>`, and every later such page. Repository pages keep `/<owner>/<name>` and its `/tree/<ref>/<path>` and `/commits/<ref>` sub-routes.
- **Amends 0021 — runs.** A run page is `/-/runs/<id>`, where `<id>` is the run's public id; a run is no longer addressed under its repository.
- **Amends 0021 — reserved owner.** `-` is never an owner or repository name. The shared validator in `kajiba-core` already refuses it, since a name starts with a letter or digit; a test keeps it that way.

## Consequences

- New application pages need no new reserved name and cannot collide with a user or repository.
- A run link stays valid when its repository is renamed, since it does not name the repository.

## Rejected alternatives

- A reserved name per page (`/login`, `/account`, …): each one takes a name away from users.
- Runs under `/<owner>/<name>/runs/<id>`: the repository part is redundant with a globally unique run id and breaks on rename.
