# ADR 0098 — One CLI credential file per forge

Status: Accepted — 2026-10-30. Amends 0089.

## Context

ADR 0089 has `kajiba login` write the token file and a `url` file beside it. Two token files in one directory share that `url` file: after logging in to forge A with one file and to forge B with another, a command using A's file sends A's token to B. The two files are also written one after the other, so a failure between them leaves a new token paired with the previous forge. The owner decided on 2026-10-30 to keep the pair in one file.

## Decision

- **Amends 0089 — credential file.** The token file holds the forge URL on its first line and the token on its second. `kajiba login` writes both at once: a temporary file in the same directory, mode 0600, renamed over the previous one, so a reader sees the old pair or the new one, never a mix. The `url` file disappears; a file without a URL on its first line is refused and asks for `kajiba login` (no migration: no user yet).
- `--token-file` selects the file; its default stays `$XDG_CONFIG_HOME/kajiba/token`.
- **Forge URL.** `kajiba login` takes the URL from `--url`, then `KAJIBA_URL`, then the file it is about to replace. Every other command calls the URL of its file; a `--url` or `KAJIBA_URL` naming another forge is refused before any call, naming both URLs.

## Consequences

- A token is only ever sent to the forge it was checked against by `login`; one file per forge, any number per directory.
- Using one forge with several files means one `kajiba login` per file.

## Rejected alternatives

- A `<token-file>.url` file per token file: two files written separately, the mixed state ADR 0089 had.
- Letting `--url` override the file's URL: the cross-forge leak this ADR closes.
