# ADR 0044 — Import credential input

Status: Accepted — 2026-10-25

## Context

ADR 0041 keeps a source's credentials out of the git child's command line, but `admin import` still received them inside the source URL: the CLI's own arguments, readable by every local user, held the credential for the whole import, and a token in a path or query parameter reached git's command line unchanged.

## Decision

- **Amends 0041 — credentials never enter a command line.** `admin import` refuses a source URL carrying userinfo (an `ssh://user@host/path` user name excepted, as it is not a secret), a query or a fragment. The refusal names the rule, never the source.
- **`--credentials-file <path>`.** Credentials for an `http`/`https` source come from a file holding `user:token` or a token alone (the user name is then `token`, which forges accept with a token). The CLI reads the file under the rules of the configuration's secret files (a regular file, not readable or writable by others, a few KiB at most) and sends the content to the server in the import request over the admin socket. The server hands it to git through the child's environment and a credential helper (0041). Neither the CLI nor the server logs it.
- The import request is the only place a credential travels; it is never echoed in a response, an error message or an event.

## Consequences

- An operator who used `https://user:token@host/repo` writes the credential to a private file and passes `--credentials-file` instead.
- Sources whose credential sits in the path or query cannot be imported; they were never safe to pass through a command line.
