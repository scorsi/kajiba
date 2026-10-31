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
    systemd.services.kajiba-runner = {
      description = "kajiba runner";
      wantedBy = [ "multi-user.target" ];
      # The runner reconnects with backoff (0105): no wait for network-online.
      after = [
        "network.target"
        "nix-daemon.socket"
      ];
      restartTriggers = [
        common.configFile
        cfg.package
      ];
      inherit (common) path;
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
