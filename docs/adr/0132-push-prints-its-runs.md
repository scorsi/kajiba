# ADR 0132 — A push prints the CI runs it created

Status: Accepted — 2026-10-31. Amends 0053.

## Context

A pusher learns the id of the run a push started only by searching for it. The runs are created by the `ci` event handler (0010) from the `push` events, and those events are written only once `receive-pack` has exited (0003, 0053): when `post-receive` runs, no event and so no run exists yet, and waiting for one in the hook would wait for its own process to exit. The owner decided on 2026-10-31.

## Decision

- **Amends 0053 — `SessionExited` answers the push's runs.** After reconciling the exited session's repository, the server releases the writer, then waits until the `ci` handler's checkpoint has passed the `push` events this session's reconciliation wrote (matched by its correlation id), and answers the runs keyed on those events (`run.event_id`) with their refs, in event order. It waits at most 2 s, polling every 50 ms, and answers no run past that bound or on a read error (logged as `push-runs-unavailable`). A session without `push` event (refused, nothing applied, CI-ineligible repository or ref) answers at once.
- **`serve-ssh` prints them.** The daemon passes the runs to the `serve-ssh` helper after the exit status and error message of the hand-off, as a third text message; the helper writes it to the SSH session's stderr before exiting, one line per run:

  `kajiba: CI run <id> on <ref> — kajiba ci watch <id>`

  Git shows these lines after the push result, without the `remote:` prefix (they do not travel through receive-pack's side band). Lines beyond the hand-off message's 64 KiB are dropped.

- Durability is unchanged: events are still written only after the process exits, and a server that cannot be reached at exit (0053's retried `SessionExited`) just prints no line.

## Consequences

- `git push` ends at most 2 s later when the `ci` handler lags; never when CI is off for the repository or the push creates no event.
- A push with CI prints the ids `kajiba ci watch` takes.

## Rejected alternatives

- Printing from `post-receive`: the runs do not exist yet; converting the push there would drop 0003's proof of completion (the process's exit).
- Pre-allocating run ids at `pre-receive`: the ids would be printed for runs that the `ci` handler may then not create (CI disabled meanwhile, refs not applied).
- Waiting until a run row appears: a push that creates no run would always wait the full bound.
