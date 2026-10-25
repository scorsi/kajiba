# ADR 0049 — Import source in transport helper arguments

Status: Accepted — 2026-10-25

## Context

ADR 0048 moved the import source out of the command lines of `admin import` and of the server's `git ls-remote` and `git fetch` children, by configuring it as the named remote `kajiba-import` in the children's environment, and on that basis declared a source whose secret sits in its path protected when given with `--source-file`. The final J2 verification found the claim too strong: git expands the named remote when it starts its transport. For an `http`/`https` source git runs `git remote-https kajiba-import <url>` and for an `ssh://` source `ssh <host> git-upload-pack '<path>'`, so the complete source, path token included, is again in a process command line, readable by every local user for the duration of the transfer, which is most of the import. Verified with `GIT_TRACE=1` on git 2.54. No git configuration keeps the URL out of the helper's arguments: `remote.<name>.vcs` passes `remote.<name>.url` as the second argument, and `url.<base>.insteadOf` rewrites the URL before it is passed.

## Decision

- **Amends 0048 — a secret in the source's path is not protected.** The product keeps the source out of the command lines it controls (the CLI's and the direct git children's), but git's transport helpers receive it as an argument. The only protected credential is the one given with `--credentials-file` (0044) for a source that carries none itself.
- **`--source-file` stays, with an honest description.** It reads the source from a private file instead of the command line; its help text and 0048's wording no longer promise that a token in the path is hidden from local users. An operator whose source authenticates through its path either accepts that exposure on the host or asks the source for a credential it can give with `--credentials-file`.
- **Correction of 0048.** "Git children never receive the source in argv" becomes: the direct children `git ls-remote` and `git fetch` name only the remote; the transport helper they start names the full source. The tests of 0048 keep asserting the former; the latter is git's behaviour, not the product's.

## Consequences

- No code path changes; the protection claim of 0048 and the help text of `--source-file` are corrected.
- A path-credential source is importable, as 0048 says, but is readable in `ps` on the host during the transfer, like any argument.

## Rejected alternatives

- Removing `--source-file`: it still keeps the source out of the CLI's own arguments and costs nothing; removing it changes the CLI and its tests for no gain in protection.
- A custom transport that consumes the URL from the environment: a `git-remote-*` helper of the product's own for one unsupported case; sources that need it have a credential path (0044) instead.
