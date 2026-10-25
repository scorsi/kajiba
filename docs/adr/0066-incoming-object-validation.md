# ADR 0066 — Incoming Git object validation

Status: Accepted — 2026-10-25

## Context

J3 security review item 13 identifies that incoming objects rely on Git's
default structural checks. Connectivity, signatures and ref policy do not
replace validation of object contents. ADR 0026 permits importing historical
repositories without checking signatures, but malformed history must still be
refused.

## Decision

- **Amends 0003 — incoming transport.** Repositories created by the git side
  have `receive.fsckObjects=true` and `transfer.fsckObjects=true`. Every
  receive-pack command also forces `receive.fsckObjects=true`, protecting
  existing repositories even when their local configuration disables it.
- **Amends 0026 and 0041 — import acceptance.** Import fetch commands force
  `transfer.fsckObjects=true` and `fetch.fsckObjects=true`; the fetch-specific
  setting prevents a repository-local false value overriding the transfer
  policy. Git's object validation errors reject the import before refs advance.
  The signature exemption for historical imports remains unchanged.
- Use Git's built-in fsck severities. No product option to bypass validation or
  additional dependency is introduced.

## Consequences

- Previously accepted malformed history can now fail to push or import and
  must be repaired at the source. Well-formed historical unsigned commits
  remain importable.
- Receive-pack rejects malformed objects in quarantine before hook admission
  and ref updates. Import keeps atomic ref updates; fetch is not a quarantine
  transaction, so a failed import can leave unreferenced objects on disk.
- Existing stored history is not retroactively scanned or rewritten. Validation
  adds work proportional to incoming object contents.

## Rejected alternatives

- Relying on Git defaults or configuring only newly created repositories:
  incoming validation would remain disabled on some repositories.
- Checking only in the pre-receive hook: imports bypass it, and Git already
  provides object validation during ingestion.
