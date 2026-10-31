# What nixosModules.runner and darwinModules.runner share (0015, 0019, 0110): the options,
# mirroring the runner's configuration fields one to one, and the generated file, checked
# by `kajiba-runner config check` at build time.
{
  config,
  lib,
  pkgs,
}:
let
  inherit (lib) mkOption types;
  cfg = config.services.kajiba-runner;
  # null means "use the code default" (0019: the module only writes what differs).
  optional =
    type: default: description:
    mkOption {
      type = types.nullOr type;
      default = null;
      description = "${description} null keeps the default (${default}).";
    };
  defaultStateDir = "/var/lib/kajiba-runner";

  settings = lib.filterAttrsRecursive (_: value: value != null) {
    inherit (cfg)
      name
      url
      hostname
      backends
      tags
      ;
    token_file = cfg.tokenFile;
    max_jobs = cfg.maxJobs;
    state_dir = if cfg.stateDir == defaultStateDir then null else cfg.stateDir;
    development = if cfg.development then true else null;
    log = { inherit (cfg.log) level; };
    metrics = { inherit (cfg.metrics) listen; };
  };
in
{
  options.services.kajiba-runner = {
    enable = lib.mkEnableOption "the kajiba runner";
    package = lib.mkPackageOption pkgs "kajiba-runner" { };
    name = mkOption {
      type = types.str;
      description = "Runner name, declared on the server (0026).";
    };
    url = mkOption {
      type = types.str;
      example = "https://forge.example.org";
      description = "Forge URL: https, or plain http to a loopback host in development mode (0104).";
    };
    tokenFile = mkOption {
      # A string, never a Nix path, so the secret is never copied to the world-readable store.
      type = types.addCheck (types.strMatching "/.+") (
        path: !(lib.hasPrefix "${builtins.storeDir}/" path) && !(builtins.hasContext path)
      );
      description = "Absolute path to the shared secret, read at runtime by the runner user (0008).";
    };
    hostname =
      optional types.str "the OS hostname"
        "Hostname reported to the server, informational (0110).";
    backends = optional (types.listOf types.str) ''[ "nix" ]'' "Execution backends announced (0009).";
    tags = optional (types.listOf types.str) "[ ]" "Tags announced (0008).";
    maxJobs = optional types.ints.positive "1" "Concurrent jobs announced.";
    stateDir = mkOption {
      type = types.strMatching "/.+";
      default = defaultStateDir;
      description = "Source cache and scratch checkouts (0106).";
    };
    development = mkOption {
      type = types.bool;
      default = false;
      description = "Development mode: allow plain http to a loopback url (0104, 0110). Never in production.";
    };
    log.level = optional types.str "info" "tracing filter, overridden by RUST_LOG (0022).";
    metrics.listen = optional types.str "no listener" "Loopback socket address of /metrics (0022).";
  };

  # Structural check only: the token file exists at runtime, not at build time (0019).
  configFile =
    let
      raw = (pkgs.formats.toml { }).generate "kajiba-runner.toml" settings;
    in
    pkgs.runCommand "kajiba-runner.toml" { } ''
      ${lib.getExe cfg.package} config check ${raw}
      cp ${raw} "$out"
    '';

  # The nix backend runs `nix`, the source cache `git`, the host check `id` (0009, 0106).
  path = [
    config.nix.package
    pkgs.git
    pkgs.coreutils
  ];
}
