# ADR 0089 — CLI forge URL and login

Status: Accepted — 2026-10-30

## Context

ADR 0007 has the `kajiba` CLI read a personal access token from a file that `kajiba login` writes. It does not say how the CLI learns which forge to call, where `login` takes the token from, nor whether it checks the token before saving it. The owner decided these points on 2026-10-30.

## Decision

- **Amends 0007 — forge URL.** The CLI calls the forge at its public URL (`http.public_url`, 0019), taken from, in order: the global `--url` option, the `KAJIBA_URL` environment variable, then the `url` file next to the token file. Without any of them, the command fails and names the three sources. An `https` URL uses TLS with the system certificate store.
- **Amends 0007 — login.** `kajiba login` reads the token from standard input, without echo when it is a terminal, never from an argument. It refuses a value outside the `kjb_` format (0014), then verifies the token with one authenticated call (`AuthService.GetViewer`) to the forge URL. Only when the call succeeds does it write the token file (directory created, mode 0600) and the forge URL into the `url` file beside it; a refused token leaves both files unchanged.

## Consequences

- A token never appears in shell history or the process list.
- `kajiba login` needs a forge URL, from `--url`, `KAJIBA_URL` or a previous login.
- A token without the `repo:read` scope cannot be saved by `login`: the J4 commands need that scope anyway.

## Rejected alternatives

- The token as a command-line argument: visible in history and `ps`.
- Saving the token without a call: a typo or a revoked token surfaces only on the next command.
- A separate CLI configuration file: the URL is the one setting, kept beside the token it belongs to.
