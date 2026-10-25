# ADR 0071 — Hook diagnostics and ref-update input limit

Status: Accepted — 2026-10-25

## Context

J3 review quality notes 13 and 14 identify two undocumented parts of the thin
hook contract: refusals are plain text for Git to show the pusher, and hook
stdin is bounded to 1 MiB. ADRs 0022 and 0052 otherwise require every stderr
line to follow the structured log schema; ADRs 0003 and 0053 do not specify
the byte limit.

## Decision

- **Amends 0022 and 0052 — hook diagnostics.** Per-ref and whole-push
  refusal reasons returned to `kajiba-git hook pre-receive` may be plain text
  on stderr, prefixed `kajiba:`, because Git forwards them to the pusher.
  This exception is limited to user-facing hook refusals. Operational logs,
  including malformed input, invalid context and RPC failures, retain the
  nisshi schema. Daemon journals remain fully structured.
- **Amends 0003 and 0053 — hook input.** Each pre-receive or post-receive
  invocation accepts at most **1,048,576 bytes** of raw stdin, including
  separators and terminating newlines. Exactly that many bytes are allowed
  if the input and context are otherwise valid. The hook reads at most one
  additional byte to detect overflow, before parsing or making an RPC.
- Oversized input makes the hook exit 1 with the structured error
  `too many ref updates`, without a hook RPC. In pre-receive this refuses
  the whole push before refs change. In post-receive refs have already
  changed: failure does not roll them back; `SessionExited` and pending-push
  reconciliation retain ADR 0053's durability guarantee.
- The limit is in bytes, not a fixed ref count. The separate gRPC message
  size limit still applies; raising either limit requires checking both.

## Consequences

- Git users receive readable policy refusals while service journals preserve
  their operational log contract.
- Boundary tests cover valid input exactly at the limit and one byte over it
  for both hook kinds, proving overflow is refused before connecting.

## Rejected alternatives

- JSON policy refusals: obscures the reasons Git forwards to the pusher.
- Unbounded stdin: allocates memory before the RPC's own message limit applies.
