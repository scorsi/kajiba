# ADR 0001 — Name and scope

Status: Accepted — 2026-10-09 (revised 2026-10-24: product wording, external Nix cache)

## Context

A self-hosted git forge for one owner, with a users and permissions model from the start so a small team can join later. No multi-tenancy, vertical scaling. It must be able to take over from an existing forge (import of repositories with their history, 0021) and then host its own development. Public repository from the first commit.

## Decision

- Name: **kajiba** (鍛冶場, the forge). Components named by suffix, one single product: `kajiba-server`, `kajiba-runner`, `kajiba-cli`, `kajiba-ui`. Code organization: see 0018.
- Scope by phases:
  - Core: git hosting (SSH + HTTP), CI runners through a home-grown protocol with interchangeable execution backends, push mirroring to GitHub, web frontend, CLI.
  - Next: issues, GitHub OAuth for the identity of outside people.
  - Later: external merge requests (branch fetched by the forge into an isolated namespace, CI never started automatically), git-LFS.
  - Out of scope until needed: wiki, releases, package registry, outgoing webhooks.
  - **Out of scope, provided by an external product: the Nix binary cache.** CI build outputs reach a cache (e.g. oroshi) through the runner hosts' own Nix configuration (0010); kajiba neither stores nor serves cache objects, and works without a cache.
- Technical milestones, no calendar.

## Consequences

- The data model, ref namespaces and permissions must accommodate issues, external identities and external contributions without a redesign (0013, 0014).
- Out-of-scope features must not be made impossible by a structural choice (LFS assumes an authenticated HTTP write endpoint: see 0004/0005).
- The documentation describes the product only; values of a particular deployment (hosts, ports, secrets, which repositories live where) belong to that deployment's repository.

## Rejected alternatives

- Tsugite, kiba, kōbō, sumitsubo, kanatoko: candidate names not retained.
- A Nix binary cache inside the forge: different resource profile (large blobs, retention, GC) and life cycle; the forge and the cache must each stay useful without the other.
