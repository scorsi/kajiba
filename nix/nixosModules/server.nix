# kajiba-server: typed options mirroring the server configuration one to one (0019), a TOML
# file generated from them and checked at build time by `config check`, loopback metrics
# (0022), fixed identity, hardened unit and persisted paths (0012, 0015, 0022).
{
  config,
  lib,
  pkgs,
  ...
}:
let
  inherit (lib) mkOption types;
  cfg = config.services.kajiba-server;

  # null means "use the code default" (0019: the module only writes what differs).
  optional =
    type: default: description:
    mkOption {
      type = types.nullOr type;
      default = null;
      description = "${description} null keeps the default (${default}).";
    };
  secretFile =
    description:
    mkOption {
      # A string, never a Nix path, so a secret is never copied into the store.
      type = types.strMatching "/.+";
      description = "Absolute path to ${description}, read at runtime by the service user (0019).";
    };
  credentials =
    description:
    mkOption {
      type = types.attrsOf (types.submodule { options.tokenFile = secretFile description; });
      default = { };
      description = "Named credentials; the secret itself is only a file path.";
    };

  settings = lib.filterAttrsRecursive (_: value: value != null) {
    state_dir = if cfg.stateDir == "/var/lib/kajiba" then null else cfg.stateDir;
    http = {
      inherit (cfg.http) listen reflection;
      public_url = cfg.http.publicUrl;
    };
    ssh = { inherit (cfg.ssh) host port; };
    log = { inherit (cfg.log) level; };
    metrics = { inherit (cfg.metrics) listen; };
    auth = {
      session_ttl = cfg.auth.sessionTtl;
      enrollment_ttl = cfg.auth.enrollmentTtl;
    };
    users = lib.mapAttrs (_: user: {
      is_admin = user.isAdmin;
      ssh_keys = user.sshKeys;
      signing_keys = user.signingKeys;
      inherit (user) tokens;
    }) cfg.users;
    runners = lib.mapAttrs (_: runner: { token_file = runner.tokenFile; }) cfg.runners;
    mirror = {
      credentials = lib.mapAttrs (_: cred: { token_file = cred.tokenFile; }) cfg.mirror.credentials;
      reconcile_interval = cfg.mirror.reconcileInterval;
    };
    ci = {
      log_retention = cfg.ci.logRetention;
      artifact_retention = cfg.ci.artifactRetention;
    };
    dump = {
      dir = if cfg.dump.dir == "/var/lib/kajiba-dumps" then null else cfg.dump.dir;
      inherit (cfg.dump) keep;
    };
  };

  # Structural check only: secret files exist at runtime, not at build time (0019).
  configFile =
    let
      raw = (pkgs.formats.toml { }).generate "kajiba-server.toml" settings;
    in
    pkgs.runCommand "kajiba-server.toml" { } ''
      ${lib.getExe cfg.package} config check ${raw}
      cp ${raw} "$out"
    '';

  # ExecStartPost probes /readyz on a connectable address, even for a wildcard listener.
  listen = if cfg.http.listen == null then "127.0.0.1:8080" else cfg.http.listen;
  probe =
    if lib.hasPrefix "0.0.0.0:" listen then
      "127.0.0.1:${lib.removePrefix "0.0.0.0:" listen}"
    else if lib.hasPrefix "[::]:" listen then
      "[::1]:${lib.removePrefix "[::]:" listen}"
    else
      listen;
  # systemd lets ExecStartPost run after the main process died, until TimeoutStartSec: stop
  # probing once $MAINPID is gone so a failed startup fails the unit at once. Silent: any
  # output would be a non-JSON line in the unit's journal (0022).
  readyProbe = pkgs.writeShellScript "kajiba-server-ready" ''
    while kill -0 "$MAINPID" 2>/dev/null; do
      ${lib.getExe pkgs.curl} --silent --fail --output /dev/null --max-time 5 "$1" && exit 0
      ${pkgs.coreutils}/bin/sleep 1
    done
    exit 1
  '';

  # systemd creates and owns directories under /var/lib; any other path is a tmpfiles rule.
  writable = [
    cfg.stateDir
    cfg.dump.dir
  ];
  inVarLib = lib.hasPrefix "/var/lib/";
in
{
  options.services.kajiba-server = {
    enable = lib.mkEnableOption "the kajiba forge server";
    package = lib.mkPackageOption pkgs "kajiba-server" { };

    stateDir = mkOption {
      type = types.strMatching "/.+";
      default = "/var/lib/kajiba";
      description = "State directory (0012 layout).";
    };
    http = {
      listen = optional types.str "127.0.0.1:8080" "Plaintext listener behind the reverse proxy (0005).";
      publicUrl = mkOption {
        type = types.str;
        example = "https://forge.example.org";
        description = "External URL: WebAuthn relying party and origin (0014), clone URLs.";
      };
      reflection = optional types.bool "false" "Serve gRPC reflection.";
    };
    ssh = {
      host = optional types.str "host of http.publicUrl" "Host shown in SSH clone URLs (0004, 0021).";
      port = optional types.port "2222" "Port shown in SSH clone URLs.";
    };
    log.level = optional types.str "info" "tracing filter, overridden by RUST_LOG (0022).";
    metrics.listen = optional types.str "no listener" "Loopback socket address of /metrics (0022).";
    auth = {
      sessionTtl = optional types.str "7d" "Session lifetime, <n>s|m|h|d (0014).";
      enrollmentTtl = optional types.str "15m" "Enrollment link lifetime, <n>s|m|h|d (0014).";
    };
    users = mkOption {
      type = types.attrsOf (
        types.submodule {
          options = {
            isAdmin = mkOption {
              type = types.bool;
              default = false;
              description = "Whether the declared user is an administrator (0014).";
            };
            sshKeys = mkOption {
              type = types.listOf types.str;
              default = [ ];
              description = "Authentication keys, OpenSSH public key lines.";
            };
            signingKeys = mkOption {
              type = types.listOf types.str;
              default = [ ];
              description = "Commit and tag signing keys, OpenSSH public key lines.";
            };
            tokens = mkOption {
              type = types.attrsOf (
                types.submodule {
                  options = {
                    sha256 = mkOption {
                      type = types.str;
                      description = "Hex SHA-256 of the personal access token; never the token itself.";
                    };
                    scopes = mkOption {
                      type = types.listOf types.str;
                      description = "Token scopes (0014).";
                    };
                    expires = mkOption {
                      type = types.nullOr types.str;
                      default = null;
                      description = "RFC 3339 expiry; null never expires.";
                    };
                  };
                }
              );
              default = { };
              description = "Declared personal access tokens, by name.";
            };
          };
        }
      );
      default = { };
      description = "Declared users, by name (0014).";
    };
    runners = credentials "the runner's shared secret (0008)";
    mirror = {
      credentials = credentials "the GitHub personal access token (0011)";
      reconcileInterval = optional types.str "1h" "Mirror reconciliation interval (0011).";
    };
    ci = {
      logRetention = optional types.str "14d" "CI log retention (0010).";
      artifactRetention = optional types.str "14d" "CI artifact retention (0010).";
    };
    persistedPaths = mkOption {
      type = types.listOf types.str;
      readOnly = true;
      default =
        map (sub: "${cfg.stateDir}/${sub}") [
          "repos"
          "db"
          "ci"
        ]
        ++ [ cfg.dump.dir ];
      description = "Paths a host with an ephemeral root must persist (0012); `tmp/` and the lock are not.";
    };
    dump = {
      dir = mkOption {
        type = types.strMatching "/.+";
        default = "/var/lib/kajiba-dumps";
        description = "Dump directory (0016).";
      };
      keep = optional types.ints.u32 "7" "Number of dumps kept (0016).";
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        # Same addresses Rust's is_loopback accepts: 127.0.0.0/8 and ::1.
        assertion =
          cfg.metrics.listen == null
          ||
            builtins.match "(127\\.[0-9]+\\.[0-9]+\\.[0-9]+|\\[(0*:)*:?(0*:)*0*1]):[0-9]+" cfg.metrics.listen
            != null;
        message = "services.kajiba-server.metrics.listen must be a loopback socket address (0022).";
      }
    ];
    # Stable path for `kajiba-server config check --files /etc/kajiba/server.toml`.
    environment.etc."kajiba/server.toml".source = configFile;
    # Root runs `kajiba-server admin enroll` over the local socket (0003, 0014).
    environment.systemPackages = [ cfg.package ];
    # Fixed identity: sshd and file ownership need it (0022), so no DynamicUser.
    users.users.kajiba = {
      isSystemUser = true;
      group = "kajiba";
    };
    users.groups.kajiba = { };
    systemd.tmpfiles.settings."10-kajiba-server" =
      lib.genAttrs (lib.filter (dir: !inVarLib dir) writable)
        (_: {
          d = {
            user = "kajiba";
            group = "kajiba";
            mode = "0750";
          };
        });
    systemd.services.kajiba-server = {
      description = "kajiba forge server";
      wantedBy = [ "multi-user.target" ];
      after = [ "network.target" ];
      restartTriggers = [ configFile ];
      serviceConfig = {
        ExecStart = "${lib.getExe cfg.package} serve --config ${configFile}";
        # The unit is active only once /readyz answers 200 (0022); TimeoutStartSec bounds it.
        ExecStartPost = "${readyProbe} http://${probe}/readyz";
        Restart = "on-failure";
        User = "kajiba";
        Group = "kajiba";
        StateDirectory = map (lib.removePrefix "/var/lib/") (lib.filter inVarLib writable);
        StateDirectoryMode = "0750";
        # /run/kajiba holds the local socket (0003).
        RuntimeDirectory = "kajiba";
        RuntimeDirectoryMode = "0750";
        ReadWritePaths = writable;
        UMask = "0027";

        # Hardening (0022). Git is spawned later: no MemoryDenyWriteExecute (PCRE JIT), and no
        # PrivateUsers, which would hide root's UID from the local socket's peer check.
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        PrivateDevices = true;
        NoNewPrivileges = true;
        CapabilityBoundingSet = "";
        AmbientCapabilities = "";
        RestrictAddressFamilies = [
          "AF_UNIX"
          "AF_INET"
          "AF_INET6"
        ];
        RestrictNamespaces = true;
        RestrictRealtime = true;
        RestrictSUIDSGID = true;
        LockPersonality = true;
        ProtectClock = true;
        ProtectControlGroups = true;
        ProtectHostname = true;
        ProtectKernelLogs = true;
        ProtectKernelModules = true;
        ProtectKernelTunables = true;
        ProtectProc = "invisible";
        RemoveIPC = true;
        SystemCallArchitectures = "native";
        SystemCallFilter = [
          "@system-service"
          "~@privileged"
        ];

        # Per-request logging (nisshi 0009).
        LogRateLimitIntervalSec = lib.mkDefault "30s";
        LogRateLimitBurst = lib.mkDefault 30000;
      };
    };
  };
}
