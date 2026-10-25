# Server module checks (0015, 0019, 0022), after oroshi's: a full-option configuration
# evaluates, its generated file passes `config check` and matches the expected TOML; invalid
# options are refused at evaluation or at build time.
{
  inputs,
  pkgs,
  system,
  kajibaServerPackage,
  kajibaGitPackage,
}:
let
  inherit (pkgs) lib;
  linux = if pkgs.stdenv.hostPlatform.isLinux then system else "aarch64-linux";
  sshKey = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAINdamAGCsQq31Uv+08lkBzoO4XLz2qYjJa8CGmj3B1Ea user@example.org";
  signingKey = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAID1AF8PoQ4lakrcKp00bfrycmCzPLsSWjMDNVfEq9GYM user@example.org";
  sha256 = "9f2c1e0a4b7d3f8e6a5c2b1d0e9f8a7b6c5d4e3f2a1b0c9d8e7f6a5b4c3d2e1f";
  # native: evaluate for this system so the generated file builds here (Darwin included).
  evaluate =
    native: extra:
    evaluateWith native [
      full
      extra
    ];
  evaluateWith =
    native: extras:
    inputs.nixpkgs.lib.nixosSystem {
      modules = [
        ../nixosModules/server.nix
        ../nixosModules/runner.nix
        {
          nixpkgs.hostPlatform = if native then system else linux;
          boot.loader.grub.enable = false;
          fileSystems."/" = {
            device = "none";
            fsType = "tmpfs";
          };
          system.stateVersion = "26.05";
          services.kajiba-git.package =
            if native then kajibaGitPackage else inputs.self.packages.${linux}.kajiba-git;
          services.kajiba-runner = {
            enable = true;
            package = inputs.self.packages.${linux}.kajiba-runner;
          };
          services.kajiba-server = {
            enable = true;
            package = if native then kajibaServerPackage else inputs.self.packages.${linux}.kajiba-server;
            http.publicUrl = lib.mkDefault "https://forge.example.org";
          };
        }
      ]
      ++ extras;
    };
  full = {
    services.kajiba-git = {
      stateDir = "/srv/kajiba-git";
      log.level = "debug";
      metrics.listen = "127.0.0.1:9433";
    };
    services.kajiba-server = {
      stateDir = "/srv/kajiba-server";
      git.socket = "/run/custom-git/local.sock";
      http = {
        listen = "127.0.0.1:8081";
        publicUrl = "https://forge.example.org";
        reflection = true;
      };
      ssh = {
        host = "ssh.example.org";
        port = 22;
      };
      log.level = "debug";
      metrics.listen = "127.0.0.1:9431";
      auth = {
        sessionTtl = "1d";
        enrollmentTtl = "30m";
      };
      users.alice = {
        isAdmin = true;
        sshKeys = [ sshKey ];
        signingKeys = [ signingKey ];
        tokens.ci = {
          inherit sha256;
          scopes = [
            "repo:read"
            "ci:trigger"
          ];
          expires = "2030-01-01T00:00:00Z";
        };
      };
      runners.build-1.tokenFile = "/run/secrets/kajiba-runner-build-1";
      mirror = {
        credentials.github.tokenFile = "/run/secrets/kajiba-github-token";
        reconcileInterval = "2h";
      };
      repos.demo = {
        owner = "alice";
        name = "demo-renamed";
        description = "A demonstration repository";
        visibility = "public";
        defaultBranch = "trunk";
        ciEnabled = true;
        previousNames = [ "alice/demo" ];
        permissions.alice = "admin";
        mirrors = [
          {
            remoteUrl = "https://github.com/example/demo.git";
            credential = "github";
          }
          {
            remoteUrl = "https://github.com/example/demo-copy.git";
            credential = "github";
            enabled = false;
          }
        ];
      };
      repos.tools = {
        owner = "alice";
        description = "";
      };
      ci = {
        logRetention = "7d";
        artifactRetention = "30d";
      };
      dump = {
        dir = "/srv/kajiba-dumps";
        keep = 3;
      };
    };
  };
  configFileOf = evaluation: evaluation.config.environment.etc."kajiba/server.toml".source;
  configFile = extra: configFileOf (evaluate true extra);
  assertionsPass = extra: lib.all (a: a.assertion) (evaluate false extra).config.assertions;
  # An unknown or inline-secret option has no NixOS declaration: evaluation itself fails.
  evaluationFails =
    extra: !(builtins.tryEval (builtins.deepSeq (configFile extra).drvPath true)).success;
  refused =
    reason: extra:
    pkgs.runCommand "kajiba-module-refused"
      { failed = pkgs.testers.testBuildFailure (configFile extra); }
      ''
        grep -F ${lib.escapeShellArg reason} "$failed/testBuildFailure.log"
        touch "$out"
      '';
  expected = (pkgs.formats.toml { }).generate "expected-kajiba-server.toml" {
    state_dir = "/srv/kajiba-server";
    git = {
      state_dir = "/srv/kajiba-git";
      socket = "/run/custom-git/local.sock";
    };
    http = {
      listen = "127.0.0.1:8081";
      public_url = "https://forge.example.org";
      reflection = true;
    };
    ssh = {
      host = "ssh.example.org";
      port = 22;
      probe = "127.0.0.1:2222";
    };
    log.level = "debug";
    metrics.listen = "127.0.0.1:9431";
    auth = {
      session_ttl = "1d";
      enrollment_ttl = "30m";
    };
    users.alice = {
      is_admin = true;
      ssh_keys = [ sshKey ];
      signing_keys = [ signingKey ];
      tokens.ci = {
        inherit sha256;
        scopes = [
          "repo:read"
          "ci:trigger"
        ];
        expires = "2030-01-01T00:00:00Z";
      };
    };
    runners.build-1.token_file = "/run/secrets/kajiba-runner-build-1";
    mirror = {
      credentials.github.token_file = "/run/secrets/kajiba-github-token";
      reconcile_interval = "2h";
    };
    repos.demo = {
      owner = "alice";
      name = "demo-renamed";
      description = "A demonstration repository";
      visibility = "public";
      default_branch = "trunk";
      ci_enabled = true;
      previous_names = [ "alice/demo" ];
      permissions.alice = "admin";
      mirrors = [
        {
          remote_url = "https://github.com/example/demo.git";
          credential = "github";
        }
        {
          remote_url = "https://github.com/example/demo-copy.git";
          credential = "github";
          enabled = false;
        }
      ];
    };
    # Unset options keep the code defaults (0019); empty collections are written as such.
    repos.tools = {
      owner = "alice";
      description = "";
      previous_names = [ ];
      permissions = { };
      mirrors = [ ];
    };
    ci = {
      log_retention = "7d";
      artifact_retention = "30d";
    };
    dump = {
      dir = "/srv/kajiba-dumps";
      keep = 3;
    };
  };
in
{
  # As in oroshi, discard the Linux toplevel context so Darwin never builds it.
  module-eval =
    assert assertionsPass { };
    assert lib.all
      (
        mode:
        assertionsPass {
          systemd.tmpfiles.settings."10-kajiba-secret"."/run/secrets/kajiba-runner-build-1".f = {
            inherit mode;
            group = "kajiba-server";
          };
        }
      )
      [
        "0600"
        "0640"
      ];
    assert lib.all
      (
        secret:
        !(assertionsPass {
          systemd.tmpfiles.settings."10-kajiba-secret"."/run/secrets/kajiba-runner-build-1".f = secret;
        })
      )
      [
        {
          mode = "0640";
          group = "kajiba-git";
        }
        {
          mode = "0660";
          group = "kajiba-server";
        }
        {
          mode = "0601";
          group = "kajiba-server";
        }
      ];
    assert
      !(assertionsPass {
        users.groups.kajiba-git.gid = 999;
        systemd.tmpfiles.settings."10-kajiba-secret"."/run/secrets/kajiba-runner-build-1".f = {
          mode = "0640";
          group = "999";
        };
      });
    assert
      !(assertionsPass {
        users.users.kajiba-git.extraGroups = [ "shared-secret" ];
        users.groups.shared-secret = { };
        systemd.tmpfiles.settings."10-kajiba-secret"."/run/secrets/kajiba-github-token".f = {
          mode = "0640";
          group = "shared-secret";
        };
      });
    assert
      !(assertionsPass {
        services.kajiba-server.runners.build-1.tokenFile = lib.mkForce "/etc/kajiba-secret";
        environment.etc.kajiba-secret = {
          text = "test credential";
          mode = "0640";
          group = "kajiba-git";
        };
      });
    # Generated unit, identity and persisted paths (0012, 0015, 0022), full then default options.
    assert
      let
        inherit ((evaluate false { }).config) systemd users services;
        unit = systemd.services.kajiba-server.serviceConfig;
      in
      !(unit ? DynamicUser)
      && unit.User == "kajiba-server"
      && unit.Group == "kajiba-server"
      && unit.StateDirectory == [ ]
      &&
        unit.ReadWritePaths == [
          "/srv/kajiba-server"
          "/srv/kajiba-dumps"
        ]
      && unit.ReadOnlyPaths == [ "/srv/kajiba-git" ]
      && unit.SupplementaryGroups == [ "kajiba-git" ]
      && unit.RuntimeDirectoryMode == "0751"
      && unit.StateDirectoryMode == "0750"
      && unit.RuntimeDirectory == "kajiba"
      && unit.UMask == "0027"
      && unit.ProtectSystem == "strict"
      && unit.ProtectHome
      && unit.PrivateTmp
      && unit.NoNewPrivileges
      && unit.CapabilityBoundingSet == ""
      &&
        unit.RestrictAddressFamilies == [
          "AF_UNIX"
          "AF_INET"
          "AF_INET6"
        ]
      && lib.elem "@system-service" unit.SystemCallFilter
      && unit.ProcSubset == "pid"
      && unit.PrivateIPC
      && unit.SocketBindDeny == "any"
      &&
        unit.SocketBindAllow == [
          "tcp:8081"
          "tcp:9431"
        ]
      && unit.LimitCORE == 0
      && unit.LimitNOFILE == 8192
      && unit.TasksMax == 1024
      && unit.MemoryMax == "2G"
      && unit.LogRateLimitIntervalSec == "30s"
      && unit.LogRateLimitBurst == 30000
      && lib.hasSuffix " http://127.0.0.1:8081/readyz" unit.ExecStartPost
      && users.users.kajiba-server.isSystemUser
      && users.users.kajiba-server.group == "kajiba-server"
      && users.users.kajiba-server.extraGroups == [ "kajiba-git" ]
      && users.groups ? kajiba-server
      && systemd.tmpfiles.settings."10-kajiba-server" ? "/srv/kajiba-server"
      && systemd.tmpfiles.settings."10-kajiba-server" ? "/srv/kajiba-dumps"
      &&
        lib.all
          (
            path:
            let
              directory = systemd.tmpfiles.settings."10-kajiba-server".${path}.d;
            in
            directory.user == "kajiba-server" && directory.group == "kajiba-server" && directory.mode == "0750"
          )
          [
            "/srv/kajiba-server"
            "/srv/kajiba-dumps"
          ]
      &&
        services.kajiba-server.persistedPaths == [
          "/srv/kajiba-server"
          "/srv/kajiba-git"
          "/srv/kajiba-dumps"
        ];
    assert
      let
        inherit ((evaluateWith false [ ]).config) systemd environment;
        unit = systemd.services.kajiba-server.serviceConfig;
      in
      unit.StateDirectory == [
        "kajiba-server"
        "kajiba-dumps"
      ]
      &&
        unit.ReadWritePaths == [
          "/var/lib/kajiba-server"
          "/var/lib/kajiba-dumps"
        ]
      && unit.ReadOnlyPaths == [ "/var/lib/kajiba-git" ]
      && systemd.tmpfiles.settings."10-kajiba-server" == { }
      && unit.SocketBindAllow == [ "tcp:8080" ]
      && lib.hasSuffix " http://127.0.0.1:8080/readyz" unit.ExecStartPost
      &&
        systemd.services.kajiba-server.restartTriggers == [
          environment.etc."kajiba/server.toml".source
          inputs.self.packages.${linux}.kajiba-server
        ]
      && lib.elem inputs.self.packages.${linux}.kajiba-server environment.systemPackages;
    assert lib.all
      (
        evaluation:
        let
          inherit (evaluation.config)
            systemd
            users
            services
            environment
            ;
          server = systemd.services.kajiba-server;
          git = systemd.services.kajiba-git;
          unit = git.serviceConfig;
          stateDir = services.kajiba-git.stateDir;
        in
        users.users.kajiba-git.isSystemUser
        && users.users.kajiba-git.group == "kajiba-git"
        && users.users.kajiba-git.extraGroups == [ ]
        && users.groups ? kajiba-git
        && !(users.users ? kajiba)
        && unit.User == "kajiba-git"
        && unit.Group == "kajiba-git"
        && !(unit ? DynamicUser)
        && !(unit ? PrivateUsers)
        && !(server.serviceConfig ? PrivateUsers)
        && unit.Type == "notify"
        && unit.KillMode == "control-group"
        && unit.Restart == "on-failure"
        &&
          unit.SystemCallFilter == [
            "@system-service"
            "~@privileged"
          ]
        &&
          server.serviceConfig.SystemCallFilter == [
            "@system-service"
            "~@privileged"
            "@chown"
          ]
        && unit.StateDirectoryMode == "0750"
        &&
          unit.StateDirectory
          == lib.optional (lib.hasPrefix "/var/lib/" stateDir) (lib.removePrefix "/var/lib/" stateDir)
        && unit.RuntimeDirectory == "kajiba-git"
        && unit.RuntimeDirectoryMode == "0750"
        && unit.ReadWritePaths == [ stateDir ]
        && unit.InaccessiblePaths == [ "-${services.kajiba-server.stateDir}" ]
        && unit.UMask == "0027"
        && unit.SocketBindDeny == "any"
        && unit.SocketBindAllow == lib.optional (services.kajiba-git.metrics.listen != null) "tcp:9433"
        && !(unit ? ExecStartPost)
        &&
          unit.ExecStart == "${lib.getExe services.kajiba-git.package} serve --config ${
            environment.etc."kajiba/git.toml".source
          }"
        &&
          git.restartTriggers == [
            environment.etc."kajiba/git.toml".source
            services.kajiba-git.package
          ]
        && lib.elem "kajiba-git.service" server.wants
        && lib.elem "kajiba-git.service" server.after
        && !(lib.elem "kajiba-server.service" git.after)
        && lib.elem evaluation.pkgs.git git.path
        && lib.elem evaluation.pkgs.git server.path
        && lib.all (key: unit.${key} == server.serviceConfig.${key}) [
          "ProtectSystem"
          "ProtectHome"
          "PrivateTmp"
          "PrivateDevices"
          "NoNewPrivileges"
          "CapabilityBoundingSet"
          "AmbientCapabilities"
          "RestrictAddressFamilies"
          "RestrictNamespaces"
          "RestrictRealtime"
          "RestrictSUIDSGID"
          "LockPersonality"
          "ProtectClock"
          "ProtectControlGroups"
          "ProtectHostname"
          "ProtectKernelLogs"
          "ProtectKernelModules"
          "ProtectKernelTunables"
          "ProtectProc"
          "RemoveIPC"
          "SystemCallArchitectures"
          "ProcSubset"
          "PrivateIPC"
          "LimitCORE"
          "LimitNOFILE"
          "TasksMax"
          "MemoryMax"
          "LogRateLimitIntervalSec"
          "LogRateLimitBurst"
        ]
        # The runner's existing dynamic identity stays attached to its own unit.
        && systemd.services.kajiba-runner.serviceConfig.DynamicUser
        && (systemd.services.kajiba-runner.serviceConfig.User or "kajiba-runner") == "kajiba-runner"
        && (systemd.services.kajiba-runner.serviceConfig.Group or "kajiba-runner") == "kajiba-runner"
        &&
          systemd.services.kajiba-runner.serviceConfig.ExecStart == lib.getExe services.kajiba-runner.package
        && services.kajiba-server.git.stateDir == stateDir
        &&
          services.kajiba-server.persistedPaths == [
            services.kajiba-server.stateDir
            stateDir
            services.kajiba-server.dump.dir
          ]
      )
      [
        (evaluate false { })
        (evaluateWith false [ ])
      ];
    assert (evaluateWith false [ ]).config.services.kajiba-git.metrics.listen == null;
    # Dedicated SSH configuration and identity, independent of the host's admin sshd.
    assert lib.all
      (
        evaluation:
        let
          inherit (evaluation.config)
            systemd
            services
            users
            environment
            networking
            ;
          sshd = systemd.services.kajiba-sshd;
          server = systemd.services.kajiba-server;
          git = systemd.services.kajiba-git;
          lines = lib.splitString "\n" environment.etc."kajiba/sshd_config".text;
        in
        services.kajiba-server.ssh.port == services.kajiba-git.ssh.port
        && lib.all (line: lib.elem line lines) [
          "Port ${toString services.kajiba-git.ssh.port}"
          "HostKey /etc/ssh/kajiba_host_ed25519_key"
          "PidFile /run/kajiba-sshd/sshd.pid"
          "AllowUsers kajiba-git"
          "AuthenticationMethods publickey"
          "PubkeyAuthentication yes"
          "PasswordAuthentication no"
          "KbdInteractiveAuthentication no"
          "HostbasedAuthentication no"
          "PermitEmptyPasswords no"
          "PermitRootLogin no"
          "UsePAM no"
          "AuthorizedKeysFile none"
          "AuthorizedKeysCommand /etc/kajiba/authorized-keys-command %f"
          "AuthorizedKeysCommandUser kajiba-git"
          "DisableForwarding yes"
          "PermitTTY no"
          "PermitTunnel no"
          "PermitUserRC no"
          "PermitUserEnvironment no"
          "SetEnv PATH=${lib.makeBinPath [ services.kajiba-git.package ]}"
        ]
        && users.users.kajiba-git.shell == evaluation.pkgs.bash
        && users.users.kajiba-git.hashedPassword == "*"
        && users.users.sshd.isSystemUser
        && users.users.sshd.group == "sshd"
        && lib.elem "multi-user.target" sshd.wantedBy
        && lib.elem "kajiba-git.service" sshd.wants
        && lib.elem "kajiba-git.service" sshd.after
        # ExecStartPost checks sshd: neither prerequisite may wait for the server.
        && lib.elem "kajiba-sshd.service" server.wants
        && lib.elem "kajiba-sshd.service" server.after
        && lib.elem "kajiba-git.service" server.after
        && !(lib.elem "kajiba-server.service" sshd.after)
        && !(lib.elem "kajiba-server.service" git.after)
        && !(lib.elem "kajiba-sshd.service" git.after)
        && !(lib.elem "kajiba-git.service" server.before)
        && !(lib.elem "kajiba-sshd.service" server.before)
        && !(lib.elem "kajiba-git.service" sshd.before)
        && !(sshd.serviceConfig ? ExecStartPost)
        && !(git.serviceConfig ? ExecStartPost)
        && !(lib.elem "sshd.service" sshd.after)
        &&
          sshd.serviceConfig.ExecStart == "${lib.getExe' evaluation.pkgs.openssh "sshd"} -D -e -f ${
            environment.etc."kajiba/sshd_config".source
          }"
        && (sshd.serviceConfig.User or "root") == "root"
        && sshd.serviceConfig.RuntimeDirectory == "kajiba-sshd"
        && sshd.serviceConfig.UMask == "0077"
        && lib.hasInfix "ssh-keygen" sshd.preStart
        && lib.elem "${lib.getExe' evaluation.pkgs.openssh "sshd"} -t -f ${environment.etc."kajiba/sshd_config".source}" (
          lib.splitString "\n" sshd.preStart
        )
        &&
          sshd.restartTriggers == [
            environment.etc."kajiba/sshd_config".source
            services.kajiba-git.package
          ]
        && lib.elem services.kajiba-git.ssh.port networking.firewall.allowedTCPPorts
      )
      [
        (evaluateWith false [ ])
        (evaluateWith false [ { services.kajiba-git.ssh.port = 2022; } ])
      ];
    assert
      let
        defaults = (evaluateWith false [ ]).config;
        custom =
          (evaluateWith false [
            {
              services.kajiba-git.ssh.port = 2022;
              services.openssh = {
                enable = true;
                ports = [ 2200 ];
                settings.PasswordAuthentication = true;
              };
            }
          ]).config;
        fullConfig = (evaluate false { }).config;
        disabled = (evaluateWith false [ { services.kajiba-server.enable = lib.mkForce false; } ]).config;
      in
      defaults.services.kajiba-git.ssh.port == 2222
      && defaults.services.kajiba-server.ssh.probe == "127.0.0.1:2222"
      && defaults.services.kajiba-server.ssh.port == 2222
      && !defaults.services.openssh.enable
      && !(defaults.systemd.services ? sshd)
      && custom.services.kajiba-git.ssh.port == 2022
      && custom.services.kajiba-server.ssh.probe == "127.0.0.1:2022"
      && custom.services.kajiba-server.ssh.port == 2022
      && custom.services.openssh.ports == [ 2200 ]
      && custom.services.openssh.settings.PasswordAuthentication
      && custom.services.openssh.authorizedKeysCommand == "none"
      && custom.systemd.services ? sshd
      && !(lib.elem "kajiba-git.service" custom.systemd.services.sshd.after)
      && fullConfig.services.kajiba-server.ssh.port == 22
      && fullConfig.services.kajiba-server.ssh.probe == "127.0.0.1:2222"
      && fullConfig.services.kajiba-git.ssh.port == 2222
      && !(disabled.systemd.services ? kajiba-sshd);
    assert
      (evaluate false { services.kajiba-git.metrics.listen = lib.mkForce "[::1]:19433"; })
      .config.systemd.services.kajiba-git.serviceConfig.SocketBindAllow == [ "tcp:19433" ];
    assert
      let
        settings = (evaluate false { }).config.systemd.tmpfiles.settings;
      in
      settings."10-kajiba-git"."/srv/kajiba-git".d.user == "kajiba-git"
      && settings."10-kajiba-git"."/srv/kajiba-git".d.group == "kajiba-git"
      && settings."10-kajiba-git"."/srv/kajiba-git".d.mode == "0750";
    assert
      (evaluate false { services.kajiba-git.stateDir = lib.mkForce "/var/lib/custom-git"; })
      .config.systemd.services.kajiba-git.serviceConfig.StateDirectory == [ "custom-git" ];
    assert
      (evaluate false { services.kajiba-server.stateDir = lib.mkForce "/var/lib/custom-server"; })
      .config.systemd.services.kajiba-server.serviceConfig.StateDirectory == [ "custom-server" ];
    assert evaluationFails { services.kajiba-git.enable = true; };
    assert evaluationFails { services.kajiba-git.unexpected = true; };
    assert evaluationFails { services.kajiba-git.stateDir = lib.mkForce "relative"; };
    assert lib.all
      (
        pair:
        lib.hasSuffix " http://${pair.probe}/readyz"
          (evaluate false {
            services.kajiba-server.http.listen = lib.mkForce pair.listen;
          }).config.systemd.services.kajiba-server.serviceConfig.ExecStartPost
      )
      [
        {
          listen = "0.0.0.0:8082";
          probe = "127.0.0.1:8082";
        }
        {
          listen = "[::]:8082";
          probe = "[::1]:8082";
        }
      ];
    assert
      (evaluate false { services.kajiba-server.dump.dir = lib.mkForce "/var/lib/kajiba-dumps"; })
      .config.systemd.services.kajiba-server.serviceConfig.StateDirectory == [ "kajiba-dumps" ];
    assert
      (evaluate false { }).config.systemd.services.kajiba-server.serviceConfig.ExecStart
      == "${lib.getExe inputs.self.packages.${linux}.kajiba-server} serve --config ${
        (evaluate false { }).config.environment.etc."kajiba/server.toml".source
      }";
    assert lib.all
      (address: !(assertionsPass { services.kajiba-server.metrics.listen = lib.mkForce address; }))
      [
        "0.0.0.0:9431"
        "192.0.2.1:9431"
        "[::]:9431"
        "[2001:db8::1]:9431"
      ];
    assert lib.all
      (address: assertionsPass { services.kajiba-server.metrics.listen = lib.mkForce address; })
      [
        "127.0.0.2:9431"
        "[::1]:9431"
      ];
    assert lib.all
      (address: !(assertionsPass { services.kajiba-git.metrics.listen = lib.mkForce address; }))
      [
        "0.0.0.0:9433"
        "192.0.2.1:9433"
        "[::]:9433"
        "[2001:db8::1]:9433"
      ];
    assert lib.all
      (address: assertionsPass { services.kajiba-git.metrics.listen = lib.mkForce address; })
      [
        "127.0.0.2:9433"
        "[::1]:9433"
      ];
    assert evaluationFails { services.kajiba-server.unexpected = true; };
    assert evaluationFails { services.kajiba-server.runners.build-1.token = "inline-secret"; };
    assert evaluationFails { services.kajiba-server.repos.demo.unexpected = true; };
    assert evaluationFails {
      services.kajiba-server.repos.demo.mirrors = lib.mkForce [
        {
          remoteUrl = "https://github.com/example/demo.git";
          credential = "github";
          token = "inline-secret";
        }
      ];
    };
    assert evaluationFails {
      services.kajiba-server.mirror.credentials.github.tokenFile = lib.mkForce ./module.nix;
    };
    # A store path, literal or interpolated (with string context), is world-readable.
    assert lib.all
      (
        tokenFile:
        evaluationFails { services.kajiba-server.runners.build-1.tokenFile = lib.mkForce tokenFile; }
      )
      [
        "${builtins.storeDir}/00000000000000000000000000000000-secret"
        "${pkgs.writeText "kajiba-secret" "DO-NOT-PRINT"}"
      ];
    assert
      !evaluationFails {
        services.kajiba-server.runners.build-1.tokenFile = lib.mkForce "/run/secrets/nix-store-lookalike";
      };
    pkgs.runCommand "kajiba-module-eval" { } ''
      cmp ${configFile { }} ${expected}
      ${lib.getExe kajibaGitPackage} config check ${
        (evaluate true { }).config.environment.etc."kajiba/git.toml".source
      }
      grep -Fx 'state_dir = "/srv/kajiba-git"' ${
        (evaluate true { }).config.environment.etc."kajiba/git.toml".source
      }
      grep -Fx 'level = "debug"' ${(evaluate true { }).config.environment.etc."kajiba/git.toml".source}
      grep -Fx 'listen = "127.0.0.1:9433"' ${
        (evaluate true { }).config.environment.etc."kajiba/git.toml".source
      }
      # The git-side default file only needs the store hook directory.
      [ "$(grep -c = ${(evaluateWith true [ ]).config.environment.etc."kajiba/git.toml".source})" = 1 ]
      grep -E '^hooks_path = "/nix/store/[^"]+-kajiba-hooks"$' ${
        (evaluateWith true [ ]).config.environment.etc."kajiba/git.toml".source
      }
      hooks=$(sed -n 's/^hooks_path = "\(.*\)"$/\1/p' ${
        (evaluateWith true [ ]).config.environment.etc."kajiba/git.toml".source
      })
      for hook in pre-receive post-receive; do
        test -x "$hooks/$hook"
        grep -Fx "exec ${lib.getExe kajibaGitPackage} hook $hook" "$hooks/$hook"
      done
      # The module enables the local sshd probe beside the required public URL.
      [ "$(grep -c = ${configFileOf (evaluateWith true [ ])})" = 2 ]
      grep -Fx 'probe = "127.0.0.1:2222"' ${configFileOf (evaluateWith true [ ])}
      grep -Fx 'port = 2022' ${
        configFileOf (evaluateWith true [ { services.kajiba-git.ssh.port = 2022; } ])
      }
      grep -Fx 'probe = "127.0.0.1:2022"' ${
        configFileOf (evaluateWith true [ { services.kajiba-git.ssh.port = 2022; } ])
      }
      ! grep -q '^probe =' ${
        configFileOf (evaluateWith true [ { services.kajiba-server.ssh.probe = null; } ])
      }
      # Parse the actual dedicated config with OpenSSH, without runtime users or host keys.
      ${lib.getExe' pkgs.openssh "sshd"} -G -f ${
        (evaluateWith true [ ]).config.environment.etc."kajiba/sshd_config".source
      } > sshd-settings
      grep -Fix 'port 2222' sshd-settings
      grep -Fix 'authenticationmethods publickey' sshd-settings
      grep -Fix 'authorizedkeyscommanduser kajiba-git' sshd-settings
      echo ${
        builtins.unsafeDiscardStringContext (evaluate false { }).config.system.build.toplevel.drvPath
      }
      touch "$out"
    '';

  # Generated file refused by `config check` although the option types accept it.
  module-metrics-listen = refused "metrics.listen: must be a loopback address" {
    services.kajiba-server.metrics.listen = lib.mkForce "0.0.0.0:9431";
  };
  module-repo-owner = refused "repos.tools.owner: not a declared user" {
    services.kajiba-server.repos.tools.owner = lib.mkForce "bob";
  };
  module-token-hash = refused "users.alice.tokens.ci.sha256" {
    services.kajiba-server.users.alice.tokens.ci.sha256 = lib.mkForce "not-a-hash";
  };
}
