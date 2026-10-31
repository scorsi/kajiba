# ADR 0123 — Runner mTLS direct dependencies

Status: Accepted — 2026-10-31. Amends 0034, 0111.

## Context

After J6 the runner channel moves from a shared secret to mutual TLS with X.509-SVIDs (0124): the forge terminates TLS itself for `kajiba.runner.v1`, both sides load their SVID, key and trust bundle from files and reload them when they change (0125), and each side verifies the peer's SPIFFE ID. The TLS stack is already in `Cargo.lock`, transitive through tonic's TLS feature (rustls with the `ring` provider) and through `webauthn-rs` (`x509-parser`). The owner allowed on 2026-10-30 that a crate already transitive becomes direct without asking, recorded in the register.

## Decision

- **Amends 0034 — the runner mTLS crates.** All four are already in `Cargo.lock`; workspace versions and features apply.

  | Crate                                              | Used by                                | Why                                                                                                                                            |
  | -------------------------------------------------- | -------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------- |
  | `rustls` 0.23 (`ring`, `std`, no default features) | `kajiba-core`, server, `kajiba-runner` | TLS 1.3 configurations, certificate resolvers and peer verifiers that reload their material (0125); `ring` is the provider tonic already pulls |
  | `tokio-rustls` 0.26 (`ring`, no default features)  | server, `kajiba-runner`                | the TLS acceptor of the runner listener and the runner's connector                                                                             |
  | `rustls-pki-types` 1 (`std`)                       | `kajiba-core`                          | PEM decoding of the SVID, key and bundle files (`PemObject`)                                                                                   |
  | `x509-parser` 0.16 (no default features)           | `kajiba-core`                          | reads what TLS verification does not check: the leaf's URI SAN (the SPIFFE ID), whether it is a CA, its `NotAfter`                             |

- **Amends 0111 — the runner drops tonic's `tls-native-roots`.** The runner trusts only the bundle file (0124), never the system store; the CLI keeps the feature (0076).
- No crate is added for certificate generation, file watching or SPIFFE: tests build their certificates from fixtures, reload compares file stamps (0125), and the SPIFFE ID rules are a few lines in `kajiba-core`.

## Consequences

- No new crate enters `Cargo.lock`.
- The rustls provider is pinned to `ring` everywhere, so no second crypto library is built.

## Rejected alternatives

- `rustls-webpki` directly for the chain check: rustls already exposes the same check (`verify_server_cert_signed_by_trust_anchor`, `WebPkiClientVerifier`).
- `rcgen` to generate test certificates: a new crate for tests that committed fixtures cover.
- A SPIFFE crate: new, and larger than the parsing it would replace.
