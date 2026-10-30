# ADR 0106 — Run sources as bundles over the runner channel

Status: Accepted — 2026-10-30. Amends 0008 and 0097.

## Context

A runner needs the commit of each job it runs. ADR 0097 has worktree runs reach the runner as a bundle through `RunnerService.FetchRunBundle`, while the runner "fetches the repository's trusted refs from the forge as for a normal run", which implies a git clone over smart HTTP: anonymous for a public repository, and a second credential or an anonymous path for anything else. The owner decided on 2026-10-30 that there is one transfer path, authenticated by the runner's own credential.

## Decision

- **Amends 0008 and 0097 — no anonymous source path.** A runner never clones or fetches over smart HTTP or SSH. It receives sources only through `RunnerService.FetchRunBundle`, authenticated like every runner call (0105), for push, manual and worktree runs alike.
- **What the server serves.** `FetchRunBundle(attempt, have)` answers only for an attempt that is the job's current one, still leased, and held by the calling runner; any other attempt is refused. The answer is a git bundle of the job's commit `--not` the `have` ids that exist in the repository; unknown or malformed ids are ignored and their number is bounded. The bundle is created read-only on the repository (0052). For a worktree run, the server sends a bundle of the stored bundle's prerequisites `--not` the `have` ids, then the stored bundle (0097).
- **Runner cache.** The runner keeps one bare cache repository per repository public id under its `state_dir`, locked while used. Before an attempt it sends the commit ids its cache holds as `have`, so the bundle is incremental; it unbundles into the cache and keeps the commit under a cache-local ref. The cache keeps refs for the 16 most recently fetched commits per repository, drops older refs and prunes unreachable objects after dropping.
- **Evaluation.** The runner checks the commit out from its cache into a scratch directory and evaluates from there (`git+file://<scratch>?rev=<sha>`, 0009); the scratch directory is removed when the attempt ends, whatever its outcome.

## Consequences

- A runner reads exactly the commits of jobs it was assigned, under its own identity; nothing else of a repository, public or private, is reachable with its secret.
- A repository's history crosses the network once per runner; later runs transfer only new objects.
- Runner disk use is bounded per repository by the 16 kept commits and their history.

## Rejected alternatives

- Anonymous clone over smart HTTP: works only for public repositories and gives runners a second, unauthenticated path to the sources.
- A per-runner read credential for git transport: a second secret and a wider read scope than the runner's assigned jobs.
- A full bundle for every attempt: resends the whole history on each run.
