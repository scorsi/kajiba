# ADR 0101 — Release baseline for `buf breaking`

Status: Accepted — 2026-10-30. Amends 0020.

## Context

ADR 0020 runs `buf breaking` against the last release tag in `nix flake check`. The flake's source has no git history, so the check could not reach a tag and only printed a skip message: it never ran. No release exists yet. The owner decided on 2026-10-30 where the baseline comes from.

## Decision

- **Amends 0020 — baseline.** The baseline is the `proto/` directory of the last release (0023: signed tag `vX.Y.Z`), pinned as the non-flake input `proto-baseline` of the repository's flake. Each release moves the input to its tag and updates the lock.
- **Before the first release** there is no such input: `buf-breaking` passes and prints `no release baseline`.
- **Failures.** Before 1.0 a reported break is informational (0020): the check prints it and passes. From 1.0 on it fails the check. A `buf` run that fails for another reason (unreadable baseline, invalid `.proto`) fails the check in every version.
- The check first runs itself on temporary copies of `proto/` (no baseline, no change, a removed field before and after 1.0, an unreadable baseline), so its logic is exercised before a baseline exists.

## Consequences

- The comparison runs in the sandbox like every other check, with no network and no git history.
- A release adds one flake input change to its bump commit.

## Rejected alternatives

- Reading the tag from git history in CI: the check would leave `nix flake check`, and a clone without tags would skip it silently.
- A copy of the released `.proto` files committed in the repository: a second source of the contract to keep in step by hand.
