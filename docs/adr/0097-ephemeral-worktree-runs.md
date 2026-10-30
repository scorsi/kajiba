# ADR 0097 — Ephemeral CI runs from a working tree

Status: Accepted — 2026-10-30. Amends 0008, 0010, 0013 (run trigger) and 0019 (configuration).

## Context

Before pushing, a developer wants the repository's checks run on a machine of another system or with capabilities their own machine lacks (a Linux runner with KVM for nixosTests from a macOS laptop). The usual answer is a Nix remote builder reached as `root` over `ssh-ng`: the client sends store paths, and the builder must list it in `trusted-users`, which is root-equivalent on the builder (a trusted user can import arbitrary unsigned store paths and change the daemon's settings). kajiba already has runners that build from source without trusting anyone (0009: the runner user is not in `trusted-users`), but a run needs a commit on a trusted ref (0010, 0013): pushing a work-in-progress branch to test it is the workflow this ADR removes.

Purpose: replace such builders. The client sends **only sources** (git objects), never store paths, so the runner places no trust in the client's store; it evaluates and builds everything itself, as for any other run.

## Decision

### Command

- `kajiba ci run --worktree [--check <name>]... [--system <system>] [--repo <owner>/<name>]`, in a clone of a declared repository.
- The repository is the one addressed by the clone's remote pointing at the configured forge (HTTP or SSH form of 0021, forge URL of 0089); none, or several different repositories, fails and names them; `--repo` overrides.
- `--system` defaults to the client's own Nix system. `--check` is repeatable; without it the run is `nix flake check` for that system.
- Output, following and exit status are those of `ci run` on a sha; `ci watch` and `ci logs` work on the run like any other.

### Capture and upload (client)

1. **Capture**: `git stash create`, or `HEAD` when the tree is clean (`stash create` prints nothing); an unborn `HEAD` fails. Like a Nix flake in a git tree, this captures tracked files (index and working tree), never untracked ones. No ref is created on the forge, and the local stash list is untouched; `git bundle create` needs a head name, so the CLI writes a transient local ref under `refs/kajiba/worktree/` and deletes it before returning.
2. **Base negotiation**: `RunService.GetWorktreeBase(repo)` returns the ids of the repository's `refs/heads/*` and `refs/tags/*`. The CLI keeps the ids present in its object database and builds `git bundle create … <captured> --not <kept ids>`: the bundle carries only what the forge lacks. With no shared id, the bundle holds the whole history and the size cap applies.
3. **Routing hint**: the CLI evaluates `requiredSystemFeatures` of the selected `checks.<system>.<name>` (all checks for the system without `--check`) and sends the union. Each feature becomes a required runner **tag** of the same name (0008), so `kvm` routes to runners declared with the tag `kvm`. The hint only routes: the runner's Nix still enforces the features, so a wrong hint yields a failed or pending job, never a trust change.
4. **Upload**: `RunService.CreateWorktreeRun`, client-streaming: a first message (repository, system, checks, features), then bundle chunks. Scope `ci:trigger`. On `BUNDLE_PREREQUISITE_MISSING` (a ref moved between steps 2 and 4) the CLI renegotiates once.

### Server

- **Authorization** before any byte of bundle is accepted, from the first message: credential with `ci:trigger` (0014, 0020) and `write` role on the repository (J3 role check), so the same people who may push may run.
- **Validation of the bundle** (stored first under the server's `tmp/`, then renamed, 0032): a git bundle with exactly one head; every prerequisite reachable from `refs/heads/*` or `refs/tags/*` of the repository (`git rev-list` against them, read only, 0052); the head's name is ignored.
- **Storage**: `ci/bundles/<run public id>.bundle` in the server's state directory. Never inside `repos/`, never unbundled by the forge, never visible to git transport, browsing or mirroring. The run row has `trigger = worktree`, `sha` = captured commit, `ref` NULL, `event_id` NULL, `triggered_by` = caller; selected checks are stored on the run (NULL = whole `nix flake check`), system and features in the job's requirements.
- **Dispatch**: one replayable job (0008) with requirements `arch`/`os` from the system, backend `nix`, tags = features. If no **declared** runner can ever match, creation is refused; a declared runner that is offline leaves the job pending, as any job.
- **Lifetime**: the bundle lives exactly as long as its run and is deleted with it. Worktree runs, with their jobs, logs and bundle, are purged `ci.worktree_retention` (default `1d`) after creation by the retention timer; a run still non-terminal at that moment is cancelled first. Dumps (0016) skip `ci/bundles/`; after a restore, a non-terminal worktree run whose bundle is gone fails with reason `bundle-missing`.

### Runner (amends 0008 and 0009)

- `RunnerService.FetchRunBundle(attempt)`, server-streaming, answered only for the attempt that is current and leased to that runner.
- The runner fetches the repository's trusted refs from the forge as for a normal run into a **scratch repository** under its `state_dir`, unbundles, evaluates the flake from that commit itself (pure evaluation, the requirements of 0009 unchanged) and runs `nix build` of `checks.<system>.<name>` for each selected check, or `nix flake check`. A ref local to the scratch repository is allowed; the scratch repository is deleted when the attempt ends.
- Logs, attempts, leases, results: unchanged (0008).

### Decided values and refusals

| Point                    | Decision                                                                         |
| ------------------------ | -------------------------------------------------------------------------------- |
| Bundle size cap          | 64 MiB of bundle bytes, a product constant; counted while streaming              |
| Active worktree runs     | at most 4 non-terminal worktree runs per user, a product constant                |
| Retention                | `ci.worktree_retention`, default `1d`, from creation; bundle purged with the run |
| Prerequisite negotiation | `GetWorktreeBase` tips, client keeps those it has, one renegotiation             |
| Check and system names   | `[A-Za-z0-9_-]+` for a check, `<arch>-<os>` for a system; anything else refused  |
| Reruns                   | none: a worktree run is not re-triggered; a new `ci run --worktree` is           |

| Situation                                          | Code                  | `ErrorInfo.reason`            |
| -------------------------------------------------- | --------------------- | ----------------------------- |
| credential without `ci:trigger`                    | `PERMISSION_DENIED`   | `SCOPE_MISSING`               |
| repository not readable by the caller              | `NOT_FOUND`           | `REPO_NOT_FOUND`              |
| caller below `write`                               | `PERMISSION_DENIED`   | `ROLE_MISSING`                |
| repository archived                                | `FAILED_PRECONDITION` | `INVALID_STATE`               |
| CI disabled on the repository                      | `FAILED_PRECONDITION` | `CI_DISABLED`                 |
| malformed check or system name                     | `INVALID_ARGUMENT`    | `INVALID_FIELD`               |
| no declared runner can match the requirements      | `FAILED_PRECONDITION` | `NO_MATCHING_RUNNER`          |
| more than 4 active worktree runs for the caller    | `RESOURCE_EXHAUSTED`  | `WORKTREE_RUNS_LIMIT`         |
| bundle above 64 MiB                                | `RESOURCE_EXHAUSTED`  | `BUNDLE_TOO_LARGE`            |
| not a bundle, or not exactly one head              | `INVALID_ARGUMENT`    | `BUNDLE_INVALID`              |
| a prerequisite not reachable from the trusted refs | `FAILED_PRECONDITION` | `BUNDLE_PREREQUISITE_MISSING` |
| write barrier (0016)                               | `UNAVAILABLE`         | `MAINTENANCE`                 |

The checks run in table order: the rows up to `WORKTREE_RUNS_LIMIT` are decided on the first message, before any bundle byte is written; a bundle refused afterwards is deleted.

## Consequences

- **0010**: a third trigger, `worktree`, next to `push` and `manual`; it never comes from an event and never needs a trusted ref. **0013**: `run.trigger` gains `worktree`; the captured commit is not in the repository, so browsing it answers `REVISION_NOT_FOUND`. A migration adds the run's checks column; `docs/schema.md` follows.
- **0019**: one new key, `ci.worktree_retention`.
- A `write` user can already run code on runners by pushing to a trusted ref; this path grants the same, without a signed commit, and is recorded under the caller's identity (`triggered_by`, run events). Build outputs are the same kind of untrusted-but-sandboxed build as a manual trigger (0010): a post-build hook may publish them, and a store path stays bound to its derivation.
- Remote builders reached as `root` are no longer needed to run a repository's checks on another system.
- Runner disk use: one scratch repository per running attempt; forge disk use: at most 4 × 64 MiB per user.

## Rejected alternatives

- Remote builder over `ssh-ng` with the client in `trusted-users`: root-equivalent on the builder; the very thing replaced.
- Pushing to a hidden ref (`refs/kajiba/tmp/*`): the object becomes reachable through git transport and gc must learn a new namespace; the bundle stays outside the repository.
- Uploading a tarball of the tree: loses the git commit Nix uses for `self` (`rev`, `lastModified`) and resends unchanged history.
- Sending the client's evaluated derivations or store paths: requires trusting the client's store, the reason for this ADR.
- A configurable size cap and per-user limit: no deployment asked for one; constants until one does.
