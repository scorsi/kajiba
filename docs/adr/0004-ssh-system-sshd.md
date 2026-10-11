# ADR 0004 — SSH: dedicated system sshd and AuthorizedKeysCommand

Status: Accepted — 2026-10-09

## Context

The user's keys are FIDO2 (`sk-ssh-ed25519@openssh.com`, touch-only). Runners and the forge's outgoing keys use classic keys (an automation cannot touch a physical key). An embedded SSH server (russh) would require verifying sk signatures ourselves.

## Decision

- A **dedicated sshd instance** (declared by the NixOS module, port distinct from admin; `:2222` taken over from Forgejo after its retirement), system user `kajiba`, no auth method other than public key.
- `AuthorizedKeysCommand` = `kajiba ssh-keys <fingerprint>`: resolves the key to a user through the DB and returns a `command="kajiba serve-ssh --user <id>",restrict <key>` line.
- `kajiba serve-ssh` reads `SSH_ORIGINAL_COMMAND`, accepts only `git-upload-pack` / `git-receive-pack` on an allowed path, checks permissions (0014) then delegates to the transport (0003).
- Supported key types: sk-ed25519, ed25519, (rsa, ecdsa tolerated) — free with OpenSSH.
- Internal `SshGateway` boundary: the sshd integration is one implementation; a home-grown embedded server remains possible later.

## Consequences

- No SSH crypto to write or maintain; OpenSSH hardening.
- Coupling to the NixOS module: acceptable, the forge is only deployed through Nix. Integration tests (nixosTest) cover the full path.
- HTTP stays read-only (0005); writes go through SSH only. LFS later will require an authenticated HTTP write endpoint: to be planned in tokens (0014), not in SSH.

## Rejected alternatives

- Embedded SSH server: a single binary, but sk key verification to validate and an attack surface carried by the project.
