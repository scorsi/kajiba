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
    tls = {
      trust_domain = cfg.tls.trustDomain;
      server_id = cfg.tls.serverId;
      svid_file = cfg.tls.svidFile;
      key_file = cfg.tls.keyFile;
      bundle_file = cfg.tls.bundleFile;
    };
    max_jobs = cfg.maxJobs;
    state_dir = if cfg.stateDir == defaultStateDir then null else cfg.stateDir;
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
      example = "https://runners.forge.example.org";
      description = "The forge's runner listener, always https (0124).";
    };
    tls = {
      trustDomain = mkOption {
        type = types.str;
        example = "example.org";
        description = "Trust domain of the runner's and the forge's SPIFFE IDs (0124).";
      };
      serverId = mkOption {
        type = types.str;
        example = "spiffe://example.org/forge/kajiba-server";
        description = "The forge's SPIFFE ID, required of its certificate (0124).";
      };
      svidFile = mkOption {
        type = types.strMatching "/.+";
        description = "The runner's X.509-SVID, leaf first, reloaded when it changes (0125).";
      };
      keyFile = mkOption {
        # A string, never a Nix path, so the key is never copied to the world-readable store.
        type = types.addCheck (types.strMatching "/.+") (
          path: !(lib.hasPrefix "${builtins.storeDir}/" path) && !(builtins.hasContext path)
        );
        description = "Absolute path to the SVID private key, read at runtime by the runner user (0124).";
      };
      bundleFile = mkOption {
        type = types.strMatching "/.+";
        description = "CA certificates the forge is verified against (0124).";
      };
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
    log.level = optional types.str "info" "tracing filter, overridden by RUST_LOG (0022).";
    metrics.listen = optional types.str "no listener" "Loopback socket address of /metrics (0022).";
  };

  # Structural check only: the SVID files exist at runtime, not at build time (0019).
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
