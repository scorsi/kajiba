# ADR 0094 — `--ui-dir` before the embedded bundle

Status: Accepted — 2026-10-30

## Context

ADR 0006 embeds the web application in the server binary, planned for J5, and lists `--ui-dir` as a troubleshooting option to serve an external bundle. J4 already needs the login and enrollment pages (0084, 0085, 0086) on the forge's origin, for WebAuthn and for the e2e harness (0015). The owner decided on 2026-10-30 to bring `--ui-dir` forward to J4; embedding stays J5.

## Decision

- **Amends 0006 — `--ui-dir` in J4.** `kajiba-server serve --ui-dir <dir>` serves the built bundle (`packages.kajiba-ui`):
  - Only a GET or HEAD that no other route answers, never a gRPC call (a request with a `content-type`): a file of the bundle at that path, or `index.html` for a path without an extension, which the SPA routes in the browser; a missing file with an extension is `404`.
  - Repository page paths (`/<owner>/<name>`, GET only) keep their redirect for a former name, then get `index.html`.
  - Paths are plain names only: `..` or any component leaving the directory is refused with `404`.
  - Without the option, nothing changes: page paths answer `404` as before.
- The files are served as they are: no precompressed variant, no cache header, until the embedded bundle of J5.

## Consequences

- The web application works on the forge's origin in J4, behind the same CORS and cookie rules as in production.
- The NixOS module does not pass `--ui-dir`: production waits for the embedded bundle.

## Rejected alternatives

- A separate static file server in front: a second origin breaks the session cookie and WebAuthn's origin check, or needs a proxy.
- `tower-http`'s file service: it brings crates the workspace does not have, for a dozen lines.
