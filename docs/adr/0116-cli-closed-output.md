# ADR 0116 — CLI output to a closed stream

Status: Accepted — 2026-10-31. Amends 0007.

## Context

ADR 0007 makes `kajiba` scriptable, which includes piping its output into another program that stops reading early (`kajiba ci logs … | head -n 1`). Rust ignores `SIGPIPE`, so a write to a pipe whose reader is gone returns an error, and the standard printing macros panic on it, which prints a panic message, or tries to, and exits 101. The owner decided on 2026-10-30 that every `kajiba` command exits quietly on a closed stdout or stderr.

## Decision

- **Closed stdout**: the first write that fails because the reader is gone (`EPIPE`) ends the process at once with status 0 and prints nothing: the reader chose to stop, and the shell reports the pipeline's status from its last command.
- **Closed stderr**: diagnostics and warnings that cannot be written are dropped; the command goes on and exits with the status it would have had (a failure stays 1).
- No command panics on output, and no error trace is printed for either case.

## Consequences

- `kajiba … | head` behaves like the usual Unix tools, without a stray message.
- A command whose stdout closes early reports success even when its output was cut: a script that needs the whole output reads it whole.

## Rejected alternatives

- Restoring the default `SIGPIPE` action: the process would be killed by the signal (status 141 in a shell) and a write on a network socket could kill it too.
- Exiting 1 on a closed stdout: `set -o pipefail` scripts using `head` would fail although nothing went wrong.
