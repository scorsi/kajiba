# ADR 0039 — RPC resource names

Status: Accepted — 2026-10-24

## Context

ADR 0020 names CRUD methods `Get/List/Create/Update/Delete`. Buf lint requires request message names to follow their RPC names and be unique within the package; bare verbs would collide as services are added. J2's repository API is read-only (0026).

## Decision

- **Amends 0020 — RPC naming.** Repository reads are `RepoService.GetRepo` and `RepoService.ListRepos`, with `GetRepoRequest` / `GetRepoResponse` and `ListReposRequest` / `ListReposResponse`. This follows the existing admin `ListRepos` pattern. Bare `Get` and `List` are not used.
- Future services follow `<Verb><Resource>`, with the resource plural for list methods. Explicit business verbs such as `TriggerRun` and `CancelRun` keep the same pattern.

## Consequences

- RPC and message names remain unambiguous within a package and satisfy buf lint. Generated clients expose the same resource-qualified names as the proto contract.
