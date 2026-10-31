# ADR 0124 — Runner identity: mutual TLS with X.509-SVIDs

Status: Accepted — 2026-10-31. Amends 0005, 0008, 0009, 0015, 0019, 0026, 0104, 0105, 0108, 0110.

## Context

A runner proves itself with a shared secret sent as bearer metadata (0105) over TLS that a reverse proxy terminates (0005). The secret is one more file per runner to generate, distribute and rotate on two hosts, and the proxy sees it in clear. The deployment now has an internal CA issuing X.509-SVIDs: short-lived certificates whose single URI SAN is a SPIFFE ID `spiffe://<trust domain>/<host>/<service>`, delivered as three files (`svid.pem`, `key.pem`, `bundle.pem`) that the CA renews in place, the bundle possibly holding more than one CA certificate during a rotation. The owner decided on 2026-10-31 that runners authenticate with these certificates, that the secret goes with no fallback, and that the runner's development mode goes too.

## Decision

- **Amends 0005 — the forge terminates TLS for `kajiba.runner.v1` only.** A second listener, `runner_listener.listen`, serves `RunnerService` and nothing else: TLS 1.3, a client certificate required, ALPN `h2`, gRPC over HTTP/2. The public port stays plaintext behind the reverse proxy for everything else and no longer serves `kajiba.runner.v1` (`UNIMPLEMENTED`). Runners reach the runner listener at a name of their own, which the reverse proxy relays as raw TCP chosen by SNI, without terminating TLS: the session is end to end between runner and forge.
- **Identities.** The forge holds `spiffe://<trust domain>/<host>/kajiba-server`, each runner `spiffe://<trust domain>/<host>/kajiba-runner`, issued by the deployment's CA; kajiba issues nothing. An ID has exactly two path segments of `A–Z a–z 0–9 . - _`, neither `.` nor `..`; the trust domain is lowercase letters, digits, `.`, `-` and `_`; no port, user, query or fragment.
- **Verification, the same on both sides.** The peer's chain verifies against the current bundle with the extended key usage of its role (`serverAuth` for the forge, `clientAuth` for a runner); the leaf is not a CA; it carries exactly one URI SAN, a valid SPIFFE ID in the configured trust domain. No DNS name is checked: SNI only routes.
  - The runner requires the forge's ID to equal its `tls.server_id`.
  - The forge requires the runner's ID to equal the `spiffe_id` of a declared runner. Two independent gates: the CA decides which host may hold an ID, the forge's configuration decides which IDs are runners.
- **Amends 0008 and 0105 — the credential is the certificate.** The shared secret, its files and the `authorization` metadata are removed. A connection's runner is the declared runner whose `spiffe_id` is the peer's ID; `Hello`'s name must be that runner's name. `FetchRunBundle` is authorized by the same mapping. Revocation is removing the declaration, or the CA no longer issuing the ID (the SVID then expires within its lifetime).
- **A stream ends with its certificate.** A `Connect` stream is cut once the peer's SVID passes its `NotAfter`; the runner reconnects with its renewed SVID. Renewal never cuts a stream before that: open connections keep their handshake.
- **Amends 0108 — refusal reasons.** A peer ID matching no declared runner, or a hello naming another runner than the ID's, is refused with `UNAUTHENTICATED` / `CREDENTIAL_INVALID` and logged `runner-refused{runner,reason="identity",spiffe_id}` (warn), not journaled. The reasons are `identity`, `disabled`, `version` and `capabilities`; `unknown` and `secret` are gone. A handshake that fails verification never reaches the service and is not logged as a refusal.
- **Amends 0019 and 0026 — server fields.**

  | Field                          | Default  | Meaning                                                           |
  | ------------------------------ | -------- | ----------------------------------------------------------------- |
  | `runner_listener.listen`       | required | address of the TLS runner listener                                |
  | `runner_listener.trust_domain` | required | trust domain of the forge's and the runners' IDs                  |
  | `runner_listener.svid_file`    | required | the forge's SVID, leaf first                                      |
  | `runner_listener.key_file`     | required | its private key (a secret path)                                   |
  | `runner_listener.bundle_file`  | required | the CA certificates runners are verified against                  |
  | `runners.<name>.spiffe_id`     | required | the runner's ID, in the trust domain; two runners never share one |

  `runner_listener` is required when a runner is declared. `runners.<name>.token_file` is removed; a runner is declared by name, `spiffe_id` and capabilities.

- **Amends 0019, 0104 and 0110 — runner fields.** `token_file` and `development` are removed; `url` is always `https`, loopback included: local development uses a test CA. A new `[tls]` table:

  | Field              | Default  | Meaning                                           |
  | ------------------ | -------- | ------------------------------------------------- |
  | `tls.trust_domain` | required | trust domain of the runner's and the forge's IDs  |
  | `tls.server_id`    | required | the forge's ID, in the trust domain               |
  | `tls.svid_file`    | required | the runner's SVID, leaf first                     |
  | `tls.key_file`     | required | its private key (a secret path)                   |
  | `tls.bundle_file`  | required | the CA certificates the forge is verified against |

  The CLI's `--development` (0104) is unchanged.

- **Amends 0009 — the host check covers the key.** The runner's startup check that build users cannot read its token file applies to `tls.key_file`.
- **Amends 0015 — module secret files.** The server module's runner token options are replaced by each runner's `spiffeId`; the runner module's `tokenFile` and development option by its `tls` options.

## Consequences

- No secret is shared between forge and runner; each host holds only its own key, renewed by the CA.
- The reverse proxy no longer sees runner traffic in clear and needs no certificate for the runner name.
- Server and runners of this version switch together: a runner of the previous version cannot connect, and the reverse proxy needs its SNI relay before the switch.
- A runner outside the CA's trust domain cannot connect; interactive enrollment of such a machine (0008) remains for later.

## Rejected alternatives

- Keeping the secret as a fallback: two credentials to secure, and the weaker one decides.
- TLS terminated by the reverse proxy, passing the client certificate in a header: the proxy would become part of the identity check, and a header is forgeable by anything reaching the plaintext port.
- Serving `kajiba.runner.v1` over TLS on the public port too: the public port has no TLS of its own (0005).
- A pattern of runner IDs (`spiffe://<trust domain>/*/kajiba-runner`): adding a runner would need only the CA's configuration, and a too broad entry there would make any host a runner.
- Checking the forge's DNS name: SVIDs carry no DNS SAN, and the ID says more than the name.
