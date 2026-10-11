# ADR 0001 — Name and scope

Status: Accepted — 2026-10-09

## Context

Personal git forge, long-running project, code written by the author. Single user, with a users/permissions model planned from the start for possible friends. No multi-tenancy, vertical scaling. Eventually replaces Forgejo (migration of kiso, kanna, sekkeizu, sekkeizu-private, rirekisho), then hosts itself. Public repository from the first commit.

## Decision

- Name: **kajiba** (鍛冶場, the forge). Components named by suffix, one single product: `kajiba-server`, `kajiba-runner`, `kajiba-cli`, `kajiba-ui`. Code organization: see 0018.
- Hosted on Forgejo first (GitHub mirror), then on kajiba itself.
- Scope by phases:
  - Core: git hosting (SSH + HTTP), CI runners through a home-grown protocol with interchangeable execution backends, push mirroring to GitHub, web frontend, CLI.
  - Next: issues, GitHub OAuth for the identity of outside people.
  - Later: external merge requests (branch fetched by the forge into an isolated namespace, CI never started automatically), git-LFS.
  - Out of scope until needed: wiki, releases, package registry, Nix binary cache, outgoing webhooks.
- Technical milestones, no calendar.

## Consequences

- The data model, ref namespaces and permissions must accommodate issues, external identities and external contributions without a redesign (0013, 0014).
- Out-of-scope features must not be made impossible by a structural choice (LFS assumes an authenticated HTTP write endpoint: see 0004/0005).

## Rejected alternatives

- Tsugite, kiba, kōbō, sumitsubo, kanatoko: candidate names not retained.
