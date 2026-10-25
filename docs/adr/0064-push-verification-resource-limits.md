# ADR 0064 — Push verification resource limits

Status: Accepted — 2026-10-25

## Context

J3 security review item 7 finds that signature checks collect unbounded Git
output and can run arbitrarily many individually timed commands. Receive-pack's
input limits (0062) and session lifetime (0063) do not stop a verification worker
in the daemon after the hook exits.

## Decision

- **Amends 0014 — signature verification.** Each push may update at most **256
  refs** and introduce at most **4096 selected commits**. Selection still uses
  all new tips minus all pre-push refs, once in quarantine. Request one extra
  commit from `rev-list` to detect overflow; refuse the whole push rather than
  silently verifying a truncated history. These limits include first pushes.
- **Amends 0053 — daemon pre-receive checks.** One **30-second absolute deadline**
  covers quarantine inspection, commit selection, fast-forward checks, commit
  signatures, refusal ancestry checks and tag signatures. It starts when the
  verification worker enters the check and never resets between commands.
  At most **16,384 Git commands** may run in that operation. Each command receives
  only the remaining time; expiration kills its process group and reaps it.
  A client-side RPC timeout alone is insufficient because it would leave the
  blocking verification worker alive.
- Drain stdout and stderr incrementally with **8 KiB read buffers**, sharing a
  **256 KiB combined output cap per command** and an **8 MiB aggregate cap per
  push**. Stop retaining output at the cap. Overflow wakes the supervisor,
  kills the process group and fails verification, including when the child
  or its descendants would otherwise keep running. Pipe collection uses the
  remaining deadline as well. These are captured-output limits, not limits on
  Git's internal allocations or kernel buffers.
- **Amends 0014 — refusal detail.** Keep the first invalid commit for each
  affected ref, naming that commit and the signature reason. Do not repeat
  ancestry checks for refs already refused or add a tag refusal to an already
  refused ref. There are at most **256 signature refusal entries**. All selected
  commits still require verification before an allowed verdict.
- Exceeding any limit fails closed with a message identifying the verification
  limit. The hook refuses the push before `GateService.PreReceive`, any pending
  push row or ref update. Normal signature failures retain the existing per-ref
  admission path. No dependency, configuration option or protocol field is added;
  import and read operations keep their existing policies.

## Consequences

- Large or expensive pushes must be split into smaller pushes. A push within the
  ref and commit limits can still exceed the command, output or time budget.
- Verification work, captured output and refusal accumulation are bounded per
  push. Concurrent sessions retain the separate limits of 0063; this is not a
  cumulative repository quota or per-user fairness policy.
- Refusals report one invalid object per ref rather than every invalid ancestor.
  No accepted history, signing requirements or recovery guarantees are weakened.

## Rejected alternatives

- Only a timeout per Git process: the number of processes remains unbounded.
- Only timing out the hook RPC: daemon workers and Git children survive it.
- Truncating output or the commit list and continuing: incomplete verification
  must never yield admission.
- Configurable limits: fixed product bounds close this finding without another
  configuration contract.
