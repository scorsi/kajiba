# ADR 0048 — Protected import source

Status: Accepted — 2026-10-25

## Context

ADR 0044 moved an import's credentials out of every command line, but the source itself still travelled as an argument: of `admin import`, then of the server's `git ls-remote` and `git fetch` children. A source that authenticates through its path (a token in the URL path, as some forges and artifact stores issue) is a credential too, and `check_source` cannot recognise one inside an arbitrary path. The J2 round-3 verification found that such a source stayed readable by every local user for the whole import, and that 0044's claim that path-credential sources "cannot be imported" was not established by the userinfo, query and fragment refusals.

## Decision

- **Amends 0044 — three protected inputs.** The credentials file (0044), the source file (below) and the git children's command lines never carry a secret.
- **`--source-file <path>`.** `admin import <repo> --source-file <path>` reads the complete source from a file under the rules of the credentials file (a regular file, not readable or writable by others, a few KiB at most) and sends it in the existing `source` field of the import request. Exactly one of the positional source and `--source-file` is given. The request, the server and git treat both the same way: `check_source` still applies (no userinfo, query or fragment), and the refusal names the rule, never the source.
- **A positional source is public.** A source given as a command argument is visible in the CLI's process arguments like any command argument. A source that carries a secret anywhere, including in its path, must be given with `--source-file`; an operator who used `https://host/<token>/repo.git` as an argument writes it to a private file instead.
- **Git children never receive the source in argv.** The server configures the source as a named remote through the child's environment (`GIT_CONFIG_COUNT`/`GIT_CONFIG_KEY_n`/`GIT_CONFIG_VALUE_n` defining `remote.kajiba-import.url`) and runs `git ls-remote kajiba-import` and `git fetch … kajiba-import`. The configuration is the command's own: nothing is written to the repository.
- **Correction of 0044.** "Sources whose credential sits in the path or query cannot be imported" becomes: a credential in the query or fragment is refused (0044); a credential in the path is accepted and is protected only when the source comes from `--source-file`.

## Consequences

- The import request does not change: the CLI reads the file and sends the source in the existing field, so `proto/` and the generated TypeScript are untouched.
- Tests assert that neither command line of an import child contains the source, and that a secret-bearing source given through a file is never echoed by the CLI on git's failure.
- The product cannot stop an operator from passing a secret-bearing source positionally; the help text and this ADR say where it ends up.

## Rejected alternatives

- Detecting tokens in the path: no rule distinguishes a token from a path segment, and a false negative leaks for the whole import.
- Reading the source from standard input: a second protected input (the credentials) already uses files, and a file keeps the same mode checks as every other secret of the product (0019).
