# ADR 0119 — Runner channel bounds

Status: Accepted — 2026-10-31. Amends 0105 and 0106.

## Context

`RunnerService` sits on the public port outside the request timeout and the `kajiba.v1` layers (0105), and authenticates each call itself. Nothing bounded what a caller costs before it is authenticated: many `Connect` streams could buffer large bodies until their hello timeout, and incomplete `FetchRunBundle` requests had no deadline. Once authenticated, a runner could start any number of bundle transfers, each running `git pack-objects`, and a transfer the runner stopped reading, or whose attempt lost its lease, kept its git child alive (0106 checks the lease only at admission). The owner decided the bounds on 2026-10-31.

## Decision

These are product constants, not configuration:

| Point                               | Value                                                                                              |
| ----------------------------------- | -------------------------------------------------------------------------------------------------- |
| Requests waiting for admission      | at most 16 server-wide, `Connect` and `FetchRunBundle` together; each admitted within 10 s         |
| Message size                        | a decoded runner message holds at most 128 KiB                                                     |
| Bundle transfers                    | at most the runner's declared `max_jobs` per runner, and 8 server-wide                             |
| Transfer without progress           | cut after 60 s without a message delivered to the runner                                           |
| Transfer of an attempt that lost it | cut once the attempt is no longer the runner's current, running, leased one (checked every second) |

- **Waiting for admission** means from the request's arrival to its response head: the hello checked and the stream registered for `Connect`, the request read and the attempt's lease checked for `FetchRunBundle`. A request beyond the 16 is refused at once, before its body is decoded, with `RESOURCE_EXHAUSTED`, reason `RUNNER_ADMISSIONS_LIMIT`; one not admitted within 10 s ends with `DEADLINE_EXCEEDED`. Admitted streams do not count.
- **Messages** above 128 KiB are refused by the decoder (`OUT_OF_RANGE`); a log chunk's 64 KiB of data (0105) fits.
- **Transfers** beyond either bound are refused with `RESOURCE_EXHAUSTED`, reason `BUNDLE_TRANSFERS_LIMIT`. A transfer holds its place until its git child is reaped.
- **Ending a transfer**: when its stream ends, nothing is delivered for 60 s, or the lease is lost, the transfer stops, its git child is killed and reaped, and the runner gets `FAILED_PRECONDITION` / `INVALID_STATE` for a lost lease, `DEADLINE_EXCEEDED` for no progress.

## Consequences

- Unauthenticated callers hold at most 16 places for at most 10 s each; they can delay runner admission, not exhaust the server.
- A runner never has more git children on the forge than jobs it may run, and none outlives its attempt's lease.
- A runner whose network cannot deliver one bundle message per minute cannot fetch sources; its attempts fail as any fetch failure does.

## Rejected alternatives

- Configurable bounds: no deployment asked for one; constants until one does.
- A per-runner bound on waiting requests: the name is only known from the hello, after decoding, and the secret only after it.
- Checking the lease only at admission (0106 as written): a transfer would outlive a lost lease and keep its git child.
