# ADR 0139 — Who sees runners

Status: Accepted — 2026-10-31.

## Context

Runs carry the name and hostname of the runner that executed their latest attempt. The web application also shows runners themselves (state, last contact, current run) and mirrors. These facts describe the forge's machinery rather than the repositories a user can read.

## Decision

- **A run shows its runner to anyone who can read it.** The run page names the runner (and the hostname it reported) of the run's attempts, as the run itself already carries them.
- **Runners, mirrors and server facts are for forge administrators.** Their state, last contact, current run, mirror state and server information are shown on the administration pages and in the runner summary of the shell only to administrators.
- A finer right for operators who are not administrators is deferred.

## Consequences

- No server change: the run messages already carry the runner name, and the administration methods require the `admin` scope and the administrator flag.
- The web application offers no link from a run to a runner page, which a non-administrator could not open.

## Rejected alternatives

- Hiding the runner from run pages: a failure is much easier to explain knowing where it ran.
- Showing runner state to every user: it reveals the fleet and its activity beyond what a repository reader needs.
