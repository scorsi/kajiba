# ADR 0076 — J4 direct dependencies

Status: Accepted — 2026-10-30

## Context

J4 adds passkey authentication, grpc-web, a scope option on every public method, the `kajiba` CLI client and an end-to-end browser harness. ADR 0034 requires each milestone's direct dependencies, their users and their reasons to be recorded in a new ADR of this theme, before any code uses them. The owner approved this list on 2026-10-30.

## Decision

- **Amends 0034 — J4 Rust dependencies.** Default features are disabled unless a feature is listed.

  | Crate                                                     | Used by                      | Why                                                                                                                                                                      |
  | --------------------------------------------------------- | ---------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
  | `webauthn-rs` 0.5 (`conditional-ui`)                      | `kajiba-server`              | WebAuthn relying party for passkey enrollment and login (0002, 0014); `conditional-ui` provides discoverable-credential login; brings OpenSSL through `webauthn-rs-core` |
  | `tonic-web` 0.12                                          | `kajiba-server`              | grpc-web on the `kajiba.v1` routes (0005, 0006), matching tonic 0.12; `tower-http` comes in transitively only                                                            |
  | `prost-reflect` 0.15                                      | `kajiba-proto`, `kajiba-cli` | reading the custom scope method option from the encoded descriptor set (prost drops extension fields); protojson output for the CLI's `--json` (`serde`)                 |
  | `tokio` 1 (workspace features)                            | `kajiba-cli`                 | async runtime of the CLI's tonic client                                                                                                                                  |
  | `tonic` 0.12 feature `tls-native-roots`                   | `kajiba-cli`                 | TLS client transport for an `https` public URL, trusting the system certificate store                                                                                    |
  | `webauthn-authenticator-rs` 0.5 (`softpasskey`), dev only | `kajiba-server`              | software authenticator for enrollment and login ceremony tests                                                                                                           |

- **Amends 0034 — J4 UI dependency.** `@playwright/test` is a development dependency of `apps/ui`, pinned to the version of the nixpkgs Playwright browsers the e2e harness uses (1.59 at the time of writing); the browsers come from nixpkgs, never downloaded by Playwright.
- CORS for grpc-web is written in the server (0020's single origin); no `tower-http` direct dependency.
- `clap` 4 and `base64` 0.22 are already workspace dependencies and need no entry.

## Consequences

- OpenSSL becomes a build and runtime dependency of `kajiba-server`; the Nix packages provide it.
- Each crate enters `Cargo.lock` with the task that first uses it; a feature or crate outside this list needs a new ADR of this theme.

## Rejected alternatives

- Hand-decoding the method option from descriptor bytes: `prost-reflect` is needed anyway for protojson, one decoder serves both.
- `tower-http` for CORS: one fixed origin does not justify a direct dependency.
