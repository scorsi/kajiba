# ADR 0088 — `tower-layer` as a direct dependency

Status: Accepted — 2026-10-30

## Context

ADR 0076 adds `tonic-web` to serve grpc-web on the `kajiba.v1` routes. Its `GrpcWebLayer` is a `tower_layer::Layer`; without `tower-layer` as a direct dependency, the only way to apply it is through tonic's server builder, a detour that hides what the code does. `tower-layer` is already in the dependency graph through tonic. The owner approved it on 2026-10-30.

## Decision

- **Amends 0076 — J4 Rust dependencies.**

  | Crate             | Used by         | Why                                                                                        |
  | ----------------- | --------------- | ------------------------------------------------------------------------------------------ |
  | `tower-layer` 0.3 | `kajiba-server` | already transitive (tonic): applies `tonic-web`'s grpc-web layer to the `kajiba.v1` routes |

## Consequences

- No new crate enters `Cargo.lock`.

## Rejected alternatives

- Building a tonic server only to apply the layer: indirect, and it couples grpc-web to the server builder's defaults.
