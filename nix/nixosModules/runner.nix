# The runner unit (0008, 0009, 0015, 0022): typed configuration checked at build time, a
# fixed identity that Nix does not trust, and hardening that keeps the daemon socket usable.
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.kajiba-runner;
  common = import ../runnerCommon.nix { inherit config lib pkgs; };
  inVarLib = lib.hasPrefix "/var/lib/";
  trusted = config.nix.settings.trusted-users or [ ];
  # The caller supplies only an operation and a canonical attempt id, never unit properties,
  # executable paths or arguments. Prefix-only manage-units polkit rules are not sufficient.
  jobProperties = {
    User = "kajiba-runner";
    Group = "kajiba-runner";
    UMask = "0077";
    ReadWritePaths = "\"${lib.replaceStrings [ "\\" "\"" ] [ "\\\\" "\\\"" ] cfg.stateDir}\"";
    ProtectSystem = "strict";
    ProtectHome = "yes";
    PrivateTmp = "yes";
    PrivateDevices = "yes";
    NoNewPrivileges = "yes";
    CapabilityBoundingSet = "";
    AmbientCapabilities = "";
    RestrictAddressFamilies = "AF_UNIX AF_INET AF_INET6";
    RestrictNamespaces = "yes";
    RestrictRealtime = "yes";
    RestrictSUIDSGID = "yes";
    LockPersonality = "yes";
    ProtectClock = "yes";
    ProtectControlGroups = "yes";
    ProtectHostname = "yes";
    ProtectKernelLogs = "yes";
    ProtectKernelModules = "yes";
    ProtectKernelTunables = "yes";
    ProtectProc = "invisible";
    RemoveIPC = "yes";
    SystemCallArchitectures = "native";
    SystemCallFilter = "@system-service";
    PrivateIPC = "yes";
    LimitCORE = "0";
    SocketBindDeny = "any";
    TimeoutStopSec = "10s";
    KillMode = "control-group";
  };
  jobBroker = pkgs.writeShellScript "kajiba-job-broker" ''
    set -eu
    IFS=' ' read -r operation attempt extra
    [[ -z "$extra" && "$attempt" =~ ^[0-7][0-9A-HJKMNP-TV-Z]{25}$ ]] || exit 1
    unit="kajiba-job-$attempt.service"
    case "$operation" in
      start)
        ${pkgs.systemd}/bin/systemd-run --quiet --collect --service-type=exec --unit="$unit" \
          ${
            lib.concatStringsSep " " (
              lib.mapAttrsToList (name: value: lib.escapeShellArg "--property=${name}=${value}") jobProperties
            )
          } \
          ${lib.escapeShellArg "--setenv=HOME=${cfg.stateDir}"} \
          ${lib.escapeShellArg "--setenv=PATH=${lib.makeBinPath common.path}"} \
          --property=SystemCallFilter=~@privileged \
          --property=StandardOutput=null --property=StandardError=journal \
          --property="ExecStopPost=${lib.getExe cfg.package} record-job $attempt" \
          -- ${lib.getExe cfg.package} execute-job "$attempt" >&2
        ;;
      stop)
        if ! ${pkgs.systemd}/bin/systemctl stop "$unit" >&2; then
          [[ "$(${pkgs.systemd}/bin/systemctl show --property=LoadState --value "$unit")" = not-found ]] || exit 1
        fi
        ;;
      *) exit 1 ;;
    esac
    printf 'ok\n'
  '';
in
{
  inherit (common) options;

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        # 0009: a trusted user could weaken the build sandbox; the runner refuses it at startup too.
        assertion =
          !(lib.any (user: lib.elem user trusted) [
            "kajiba-runner"
            "@kajiba-runner"
            "*"
          ]);
        message = "services.kajiba-runner: the kajiba-runner user must not be in nix.settings.trusted-users (0009).";
      }
      {
        assertion =
          cfg.metrics.listen == null
          ||
            builtins.match "(127\\.[0-9]+\\.[0-9]+\\.[0-9]+|\\[(0*:)*:?(0*:)*0*1]):[0-9]+" cfg.metrics.listen
            != null;
        message = "services.kajiba-runner.metrics.listen must be a loopback socket address (0022).";
      }
    ];
    # Stable path for `kajiba-runner config check --files /etc/kajiba/runner.toml`.
    environment.etc."kajiba/runner.toml".source = common.configFile;
    users.users.kajiba-runner = {
      isSystemUser = true;
      group = "kajiba-runner";
    };
    users.groups.kajiba-runner = { };
    systemd.tmpfiles.settings."10-kajiba-runner" = lib.optionalAttrs (!inVarLib cfg.stateDir) {
      ${cfg.stateDir}.d = {
        user = "kajiba-runner";
        group = "kajiba-runner";
        mode = "0700";
      };
    };
    # Socket activation is the narrow root authority for starting/stopping job units.
    # It does not depend on the runner service and survives its restart.
    systemd.sockets.kajiba-job = {
      wantedBy = [ "sockets.target" ];
      socketConfig = {
        ListenStream = "/run/kajiba-job/control";
        Accept = true;
        SocketUser = "kajiba-runner";
        SocketGroup = "kajiba-runner";
        SocketMode = "0600";
        DirectoryMode = "0755";
        MaxConnections = 16;
      };
    };
    systemd.services."kajiba-job@" = {
      serviceConfig = {
        ExecStart = jobBroker;
        StandardInput = "socket";
        StandardOutput = "socket";
        StandardError = "journal";
        User = "root";
        NoNewPrivileges = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        CapabilityBoundingSet = "";
        RestrictAddressFamilies = [ "AF_UNIX" ];
        TimeoutStartSec = 30;
        RuntimeMaxSec = 30;
      };
    };
    systemd.services.kajiba-runner = {
      description = "kajiba runner";
      wantedBy = [ "multi-user.target" ];
      requires = [ "kajiba-job.socket" ];
      # The runner reconnects with backoff (0105): no wait for network-online.
      after = [
        "kajiba-job.socket"
        "network.target"
        "nix-daemon.socket"
      ];
      restartTriggers = [
        common.configFile
        cfg.package
      ];
      path = common.path ++ [ pkgs.systemd ];
      # Nix keeps its fetcher and evaluation caches under $HOME.
      environment.HOME = cfg.stateDir;
      serviceConfig = {
        ExecStart = "${lib.getExe cfg.package} --config ${common.configFile}";
        Restart = "on-failure";
        RestartSec = 5;
        User = "kajiba-runner";
        Group = "kajiba-runner";
        StateDirectory = lib.optional (inVarLib cfg.stateDir) (lib.removePrefix "/var/lib/" cfg.stateDir);
        StateDirectoryMode = "0700";
        ReadWritePaths = [ cfg.stateDir ];
        UMask = "0077";

        # Hardening (0022). Connecting to the Nix daemon socket needs AF_UNIX only: a socket
        # stays connectable on a read-only mount. Evaluation reads /proc system files and
        # spawns Git, so no ProcSubset, MemoryDenyWriteExecute or PrivateUsers.
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
        PrivateIPC = true;
        LimitCORE = 0;
        # Only the metrics listener may bind.
        SocketBindDeny = "any";
        SocketBindAllow = lib.optional (cfg.metrics.listen != null) (
          "tcp:${lib.last (lib.splitString ":" cfg.metrics.listen)}"
        );
      };
    };
  };
}
