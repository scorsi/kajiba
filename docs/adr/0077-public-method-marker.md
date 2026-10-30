# ADR 0077 — Public method marker

Status: Accepted — 2026-10-30

## Context

ADR 0014 requires every RPC to declare its required scope as a proto method option, and a unit test to fail on any method without one. `HealthService.Check` is a probe usable by any client (0005, 0020) and must stay callable without a credential, which no scope expresses.

## Decision

- **Amends 0014 and 0020 — scope or public.** `proto/kajiba/v1/options.proto` defines the `Scope` enum (`SCOPE_REPO_READ`, `SCOPE_REPO_WRITE`, `SCOPE_CI_TRIGGER`, `SCOPE_ADMIN`, the configuration's `repo:read`, `repo:write`, `ci:trigger`, `admin`) and two `MethodOptions` extensions: `scope` and `public_access`. Every `kajiba.v1` method sets exactly one of them: a scope other than `SCOPE_UNSPECIFIED`, or `public_access = true`, which means the method is callable without a credential.
- `HealthService.Check` is `public_access`; every `RepoService` method requires `repo:read`.
- The unit test accepts exactly a scope or the public marker; a method with neither, both, `SCOPE_UNSPECIFIED` or `public_access = false` fails it.

## Consequences

- Making a method public is an explicit, reviewable line in the proto, never the absence of an option.

## Rejected alternatives

- Giving `Check` a scope: probes would need a token.
- Exempting `HealthService` by name in the interceptor: the exception would be invisible in the contract.
