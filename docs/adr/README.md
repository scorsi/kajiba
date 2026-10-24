# ADR — kajiba (鍛冶場)

Architecture decisions of the forge. Format: status, context, decision, consequences, rejected alternatives. **Until implementation starts (J1), ADRs are revised in place** and the revision is dated in their status line; from then on an accepted ADR is not modified, it is superseded by a new one that references it.

Statuses: **Accepted** (decided by the owner: 2026-10-09, 2026-10-11, revisions of 2026-10-24), **Proposed** (to confirm), **Open** (point left for later).

The ADRs describe the product. Values of a particular deployment (hosts, ports, secrets, which repositories it hosts, off-host backup) live in that deployment's repository.

| No.  | Title                                                                        | Status                    |
| ---- | ---------------------------------------------------------------------------- | ------------------------- |
| 0001 | Name and scope                                                               | Accepted                  |
| 0002 | Language: Rust for the server, the runner and the CLI                        | Accepted                  |
| 0003 | Git foundation: library for reads, git binary for transport, hooks           | Accepted                  |
| 0004 | SSH: dedicated system sshd and AuthorizedKeysCommand                         | Accepted                  |
| 0005 | API: gRPC, grpc-web for the browser, a single port                           | Accepted                  |
| 0006 | Frontend: static SvelteKit embedded in the binary                            | Accepted                  |
| 0007 | CLI: Rust, client from `kajiba-proto`                                        | Accepted                  |
| 0008 | Forge ↔ runner protocol: bidirectional gRPC, declarative enrollment          | Accepted                  |
| 0009 | Interchangeable execution backends: nix → nspawn → microvm                   | Accepted                  |
| 0010 | Pipelines: Nix convention with no file at first, never Actions compatibility | Accepted                  |
| 0011 | Push mirroring to GitHub                                                     | Accepted                  |
| 0012 | Storage: SQLite, on-disk layout, ephemeral roots                             | Accepted                  |
| 0013 | Data model and ref namespaces                                                | Accepted                  |
| 0014 | Identity, authentication and permissions                                     | Accepted (UI merge: Open) |
| 0015 | Nix: flake outputs, modules, tests, self-hosting                             | Accepted                  |
| 0016 | Backup: consistent dump under a write barrier                                | Accepted                  |
| 0017 | Internal events, notifications, webhooks                                     | Accepted                  |
| 0018 | Code organization: monorepo                                                  | Accepted                  |
| 0019 | Configuration and runtime secrets                                            | Accepted                  |
| 0020 | gRPC and proto conventions                                                   | Accepted                  |
| 0021 | Repository addressing and URLs                                               | Accepted                  |
| 0022 | Operations and observability                                                 | Accepted                  |
| 0023 | Versions, migrations and compatibility                                       | Accepted                  |
| 0024 | Decisions taken during J1                                                    | Accepted                  |

Relational schema: [`docs/schema.md`](../schema.md) (Accepted).

Outside the ADRs, decided on 2026-10-11: **Apache-2.0** license from the first commit; code, comments, UI, protos, README, CLAUDE.md and CONTRIBUTING in **English**; ADRs drafted in French and translated to English before the first public push (done).

Point left open: merge from the UI (0014).
