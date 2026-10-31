# ADR 0128 — The NixOS module serves the web application

Status: Accepted — 2026-10-31. Amends 0094.

## Context

ADR 0094 brought `kajiba-server serve --ui-dir` forward to J4 but kept it out of the NixOS module: production was to wait for the bundle embedded in J5 (0006). A forge deployed through the module therefore answers `404` on every page path, login and enrollment included, until J5.

## Decision

- **Amends 0094 — the module passes `--ui-dir`.** `services.kajiba-server.ui.enable` (default `true`) appends `--ui-dir <services.kajiba-server.ui.package>` to the unit's `serve` command.
- `services.kajiba-server.ui.package` is a package option like `package`: its default is `pkgs.kajiba-ui`, and the deployment repository sets it to the flake's `packages.<system>.kajiba-ui` as it does for the server and git packages.
- The bundle is read from the Nix store, which the unit's hardening already leaves readable; no path option is added.

## Consequences

- A module deployment serves the login, enrollment and application pages on the forge's origin, with 0094's serving rules.
- A deployment that does not provide `kajiba-ui` fails at evaluation instead of serving `404`, unless it sets `ui.enable = false`.

## Rejected alternatives

- Waiting for J5: the deployed forge has no usable web interface until then.
- Defaulting `ui.package` to the flake's own package inside the module: the module does not see the flake, and the other package options already follow `pkgs`.
