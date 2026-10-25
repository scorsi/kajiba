# The paired server and git-side units (0052, 0054): typed configuration, build-time
# validation, fixed identities, separate state directories and common hardening.
{
  config,
  lib,
  pkgs,
  ...
}:
let
  inherit (lib) mkOption types;
  cfg = config.services.kajiba-server;
  gitCfg = config.services.kajiba-git;

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
      # A string, never a Nix path, so a secret is never copied into the store; a store path
      # (literal or interpolated) is refused too: the store is world-readable.
      type = types.addCheck (types.strMatching "/.+") (
        path: !(lib.hasPrefix "${builtins.storeDir}/" path) && !(builtins.hasContext path)
      );
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
    state_dir = if cfg.stateDir == "/var/lib/kajiba-server" then null else cfg.stateDir;
    git = {
      state_dir = if cfg.git.stateDir == "/var/lib/kajiba-git" then null else cfg.git.stateDir;
      socket = if cfg.git.socket == "/run/kajiba-git/local.sock" then null else cfg.git.socket;
    };
    http = {
      inherit (cfg.http) listen reflection;
      public_url = cfg.http.publicUrl;
    };
    ssh = {
      inherit (cfg.ssh) host probe;
      port = if cfg.ssh.port == 2222 then null else cfg.ssh.port;
    };
    log = { inherit (cfg.log) level; };
    metrics = { inherit (cfg.metrics) listen; };
    auth = {
      session_ttl = cfg.auth.sessionTtl;
      enrollment_ttl = cfg.auth.enrollmentTtl;
    };
    users = lib.mapAttrs (_: user: {
      is_admin = user.isAdmin;
      ssh_keys = map lib.trim user.sshKeys;
      signing_keys = map lib.trim user.signingKeys;
      inherit (user) tokens;
    }) cfg.users;
    runners = lib.mapAttrs (_: runner: { token_file = runner.tokenFile; }) cfg.runners;
    mirror = {
      credentials = lib.mapAttrs (_: cred: { token_file = cred.tokenFile; }) cfg.mirror.credentials;
      reconcile_interval = cfg.mirror.reconcileInterval;
    };
    repos = lib.mapAttrs (_: repo: {
      inherit (repo)
        owner
        name
        description
        visibility
        permissions
        ;
      default_branch = repo.defaultBranch;
      ci_enabled = repo.ciEnabled;
      previous_names = repo.previousNames;
      # filterAttrsRecursive does not enter lists.
      mirrors = map (
        mirror:
        lib.filterAttrs (_: value: value != null) {
          remote_url = mirror.remoteUrl;
          inherit (mirror) credential enabled;
        }
      ) repo.mirrors;
    }) cfg.repos;
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

  hooksPath = pkgs.runCommand "kajiba-hooks" { } ''
    mkdir "$out"
    for hook in pre-receive post-receive; do
      cat > "$out/$hook" <<HOOK
    #!${pkgs.runtimeShell}
    exec ${lib.getExe gitCfg.package} hook $hook
    HOOK
      chmod +x "$out/$hook"
    done
  '';
  gitConfigFile =
    let
      raw = (pkgs.formats.toml { }).generate "kajiba-git.toml" (
        lib.filterAttrsRecursive (_: value: value != null) {
          state_dir = if gitCfg.stateDir == "/var/lib/kajiba-git" then null else gitCfg.stateDir;
          hooks_path = toString hooksPath;
          log = { inherit (gitCfg.log) level; };
          metrics = { inherit (gitCfg.metrics) listen; };
        }
      );
    in
    pkgs.runCommand "kajiba-git.toml" { } ''
      ${lib.getExe gitCfg.package} config check ${raw}
      cp ${raw} "$out"
    '';

  sshdConfig = config.environment.etc."kajiba/sshd_config".source;
  sshHostKey = "/etc/ssh/kajiba_host_ed25519_key";

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
  hardening = {
    # Hardening (0022, 0052). Both units spawn Git: no MemoryDenyWriteExecute (PCRE JIT), and no
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
    ProcSubset = "pid";
    PrivateIPC = true;
    # Resource limits: no core dump may keep an in-memory enrollment token; the server
    # itself caps public connections at 1024.
    LimitCORE = 0;
    LimitNOFILE = lib.mkDefault 8192;
    TasksMax = lib.mkDefault 1024;
    MemoryMax = lib.mkDefault "2G";

    # Per-request logging (nisshi 0009).
    LogRateLimitIntervalSec = lib.mkDefault "30s";
    LogRateLimitBurst = lib.mkDefault 30000;
  };
in
{
  options.services.kajiba-server = {
    enable = lib.mkEnableOption "the kajiba forge server";
    package = lib.mkPackageOption pkgs "kajiba-server" { };

    stateDir = mkOption {
      type = types.strMatching "/.+";
      default = "/var/lib/kajiba-server";
      description = "State directory (0012 layout).";
    };
    git = {
      stateDir = mkOption {
        type = types.strMatching "/.+";
        default = gitCfg.stateDir;
        description = "Git-side state directory, read-only to the server (0052).";
      };
      socket = mkOption {
        type = types.strMatching "/.+";
        default = "/run/kajiba-git/local.sock";
        description = "Git-side local gRPC socket (0053).";
      };
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
      probe = mkOption {
        type = types.nullOr types.str;
        default = "127.0.0.1:${toString gitCfg.ssh.port}";
        defaultText = lib.literalExpression ''"127.0.0.1:''${toString config.services.kajiba-git.ssh.port}"'';
        description = "Local loopback sshd address checked by /readyz with a one-second SSH banner probe. null disables the check; independent of the advertised host and port.";
      };
      port = mkOption {
        type = types.nullOr types.port;
        default = gitCfg.ssh.port;
        defaultText = lib.literalExpression "config.services.kajiba-git.ssh.port";
        description = "Port shown in SSH clone URLs; defaults to the dedicated sshd port. null keeps the code default (2222).";
      };
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
              description = "Authentication keys, OpenSSH public key lines; surrounding whitespace, such as the trailing newline of `builtins.readFile`, is removed.";
            };
            signingKeys = mkOption {
              type = types.listOf types.str;
              default = [ ];
              description = "Commit and tag signing keys, OpenSSH public key lines; surrounding whitespace is removed.";
            };
            tokens = mkOption {
              type = types.attrsOf (
                types.submodule {
                  options = {
                    sha256 = mkOption {
                      type = types.str;
                      description = ''
                        Hex SHA-256 of the personal access token; never the token itself. The token
                        must carry at least 128 bits of randomness (the hash is world-readable):
                        `t="kjb_$(head -c 32 /dev/urandom | basenc --base64url | tr -d '=')"; echo "$t"; printf %s "$t" | sha256sum`.
                      '';
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
    repos = mkOption {
      type = types.attrsOf (
        types.submodule {
          options = {
            owner = mkOption {
              type = types.str;
              description = "Owner, a declared user.";
            };
            name = optional types.str "the attribute name" "Repository name (0021).";
            description = mkOption {
              type = types.str;
              description = "Free-text description.";
            };
            visibility = optional types.str "public" "Visibility; only public exists (0013).";
            defaultBranch = optional types.str "main" "Default branch, the bare repository's HEAD.";
            ciEnabled = optional types.bool "false" "Whether pushes trigger CI (0010).";
            previousNames = mkOption {
              type = types.listOf types.str;
              default = [ ];
              description = "Former `<owner>/<name>` pairs kept as redirects (0021).";
            };
            permissions = mkOption {
              type = types.attrsOf types.str;
              default = { };
              description = "Role (read, write or admin) per declared user; the owner is admin.";
            };
            mirrors = mkOption {
              type = types.listOf (
                types.submodule {
                  options = {
                    remoteUrl = mkOption {
                      type = types.str;
                      description = "Push mirror remote URL (0011).";
                    };
                    credential = mkOption {
                      type = types.str;
                      description = "Name of a mirror.credentials entry.";
                    };
                    enabled = optional types.bool "true" "Whether the mirror syncs.";
                  };
                }
              );
              default = [ ];
              description = "Push mirrors, configuration part (0011).";
            };
          };
        }
      );
      default = { };
      description = "Declared repositories, by stable key (0026): renaming changes `name`, never the key.";
    };
    ci = {
      logRetention = optional types.str "14d" "CI log retention (0010).";
      artifactRetention = optional types.str "14d" "CI artifact retention (0010).";
    };
    persistedPaths = mkOption {
      type = types.listOf types.str;
      readOnly = true;
      # Persist each whole state directory: git-side quarantine uses same-filesystem renames.
      default = [
        cfg.stateDir
        gitCfg.stateDir
        cfg.dump.dir
      ];
      description = "Paths a host with an ephemeral root must persist (0012): both whole state directories and dump.dir.";
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

  options.services.kajiba-git = {
    package = lib.mkPackageOption pkgs "kajiba-git" { };
    stateDir = mkOption {
      type = types.strMatching "/.+";
      default = "/var/lib/kajiba-git";
      description = "Git-side state directory: repositories, staging and quarantine (0052).";
    };
    log.level = optional types.str "info" "tracing filter, overridden by RUST_LOG (0022).";
    metrics.listen = optional types.str "no listener" "Loopback socket address of /metrics (0054).";
    ssh.port = mkOption {
      type = types.port;
      default = 2222;
      description = "Dedicated public-key-only Git SSH listener (0004, 0054).";
    };
  };

  config = lib.mkIf cfg.enable {
    assertions =
      map
        (service: {
          # Same addresses Rust's is_loopback accepts: 127.0.0.0/8 and ::1.
          assertion =
            config.services.${service}.metrics.listen == null
            ||
              builtins.match "(127\\.[0-9]+\\.[0-9]+\\.[0-9]+|\\[(0*:)*:?(0*:)*0*1]):[0-9]+"
                config.services.${service}.metrics.listen != null;
          message = "services.${service}.metrics.listen must be a loopback socket address (0022, 0054).";
        })
        [
          "kajiba-server"
          "kajiba-git"
        ];
    # Stable path for `kajiba-server config check --files /etc/kajiba/server.toml`.
    environment.etc."kajiba/server.toml".source = configFile;
    # Root runs `kajiba-server admin enroll` over the local socket (0003, 0014).
    environment.etc."kajiba/git.toml".source = gitConfigFile;
    environment.etc."kajiba/sshd_config".text = ''
      Port ${toString gitCfg.ssh.port}
      HostKey ${sshHostKey}
      PidFile /run/kajiba-sshd/sshd.pid
      AllowUsers kajiba-git
      AuthenticationMethods publickey
      PubkeyAuthentication yes
      PasswordAuthentication no
      KbdInteractiveAuthentication no
      HostbasedAuthentication no
      PermitEmptyPasswords no
      PermitRootLogin no
      UsePAM no
      AuthorizedKeysFile none
      AuthorizedKeysCommand ${lib.getExe gitCfg.package} ssh-keys %f
      AuthorizedKeysCommandUser kajiba-git
      DisableForwarding yes
      PermitTTY no
      PermitTunnel no
      PermitUserRC no
      PermitUserEnvironment no
      SetEnv PATH=${lib.makeBinPath [ gitCfg.package ]}
    '';
    environment.systemPackages = [
      cfg.package
      gitCfg.package
    ];
    # Fixed identity: sshd and file ownership need it (0022), so no DynamicUser.
    users.users.kajiba-server = {
      isSystemUser = true;
      group = "kajiba-server";
      extraGroups = [ "kajiba-git" ];
    };
    users.groups.kajiba-server = { };
    users.users.kajiba-git = {
      isSystemUser = true;
      group = "kajiba-git";
      # OpenSSH needs a shell and an unlocked account for the key's forced command.
      shell = pkgs.bash;
      hashedPassword = "*";
    };
    users.groups.kajiba-git = { };
    # Privilege separation also works when the administration sshd is disabled.
    users.users.sshd = {
      isSystemUser = true;
      group = "sshd";
      description = "SSH privilege separation user";
    };
    users.groups.sshd = { };
    networking.firewall.allowedTCPPorts = [ gitCfg.ssh.port ];
    systemd.services.kajiba-sshd = {
      description = "kajiba Git SSH gateway";
      wantedBy = [ "multi-user.target" ];
      wants = [ "kajiba-git.service" ];
      after = [
        "network.target"
        "kajiba-git.service"
      ];
      restartTriggers = [
        sshdConfig
        gitCfg.package
      ];
      environment.LD_LIBRARY_PATH = config.system.nssModules.path;
      preStart = ''
        if ! test -e ${sshHostKey}; then
          install -d -m 0755 /etc/ssh
          ${lib.getExe' pkgs.openssh "ssh-keygen"} -q -t ed25519 -N "" -f ${sshHostKey}
        fi
        ${lib.getExe' pkgs.openssh "sshd"} -t -f ${sshdConfig}
      '';
      serviceConfig = {
        ExecStart = "${lib.getExe' pkgs.openssh "sshd"} -D -e -f ${sshdConfig}";
        Restart = "on-failure";
        RuntimeDirectory = "kajiba-sshd";
        RuntimeDirectoryMode = "0755";
        UMask = "0077";
      };
    };
    systemd.tmpfiles.settings."10-kajiba-server" =
      lib.genAttrs (lib.filter (dir: !inVarLib dir) writable)
        (_: {
          d = {
            user = "kajiba-server";
            group = "kajiba-server";
            mode = "0750";
          };
        });
    systemd.services.kajiba-server = {
      description = "kajiba forge server";
      wantedBy = [ "multi-user.target" ];
      wants = [
        "kajiba-git.service"
        "kajiba-sshd.service"
      ];
      # sshd must start before ExecStartPost waits for its readiness probe.
      after = [
        "network.target"
        "kajiba-git.service"
        "kajiba-sshd.service"
      ];
      restartTriggers = [
        configFile
        cfg.package
      ];
      # Smart HTTP upload-pack and mirror pushes read repositories (0052).
      path = [ pkgs.git ];
      serviceConfig = hardening // {
        ExecStart = "${lib.getExe cfg.package} serve --config ${configFile}";
        # The unit is active only once /readyz answers 200 (0022); TimeoutStartSec bounds it.
        ExecStartPost = "${readyProbe} http://${probe}/readyz";
        Restart = "on-failure";
        User = "kajiba-server";
        Group = "kajiba-server";
        StateDirectory = map (lib.removePrefix "/var/lib/") (lib.filter inVarLib writable);
        StateDirectoryMode = "0750";
        # /run/kajiba holds the local socket (0003).
        RuntimeDirectory = "kajiba";
        RuntimeDirectoryMode = "0751";
        ReadWritePaths = writable;
        ReadOnlyPaths = [ cfg.git.stateDir ];
        SupplementaryGroups = [ "kajiba-git" ];
        # Assign git.sock to the supplementary group; DAC still applies with no capabilities.
        SystemCallFilter = hardening.SystemCallFilter ++ [ "@chown" ];
        UMask = "0027";

        # Only the configured listeners may bind (the local socket is AF_UNIX, not filtered).
        SocketBindDeny = "any";
        SocketBindAllow = map (addr: "tcp:${lib.last (lib.splitString ":" addr)}") (
          [ listen ] ++ lib.optional (cfg.metrics.listen != null) cfg.metrics.listen
        );
      };
    };
    systemd.tmpfiles.settings."10-kajiba-git" = lib.optionalAttrs (!inVarLib gitCfg.stateDir) {
      ${gitCfg.stateDir}.d = {
        user = "kajiba-git";
        group = "kajiba-git";
        mode = "0750";
      };
    };
    systemd.services.kajiba-git = {
      description = "kajiba repository writer";
      wantedBy = [ "multi-user.target" ];
      after = [ "network.target" ];
      restartTriggers = [
        gitConfigFile
        gitCfg.package
      ];
      path = [
        pkgs.git
        pkgs.openssh
      ];
      serviceConfig = hardening // {
        ExecStart = "${lib.getExe gitCfg.package} serve --config ${gitConfigFile}";
        Type = "notify";
        KillMode = "control-group";
        Restart = "on-failure";
        User = "kajiba-git";
        Group = "kajiba-git";
        StateDirectory = lib.optional (inVarLib gitCfg.stateDir) (
          lib.removePrefix "/var/lib/" gitCfg.stateDir
        );
        StateDirectoryMode = "0750";
        RuntimeDirectory = "kajiba-git";
        RuntimeDirectoryMode = "0750";
        ReadWritePaths = [ gitCfg.stateDir ];
        # The server's directory may not exist yet on the first boot (git starts first).
        InaccessiblePaths = [ "-${cfg.stateDir}" ];
        UMask = "0027";
        SocketBindDeny = "any";
        SocketBindAllow = lib.optional (gitCfg.metrics.listen != null) (
          "tcp:${lib.last (lib.splitString ":" gitCfg.metrics.listen)}"
        );
      };
    };
  };
}
