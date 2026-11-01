# Server and runner module checks (0015, 0019, 0022), after oroshi's: a full-option configuration
# evaluates, its generated file passes `config check` and matches the expected TOML; invalid
# options are refused at evaluation or at build time.
{
  inputs,
  pkgs,
  system,
  kajibaServerPackage,
  kajibaGitPackage,
  kajibaRunnerPackage,
  kajibaUiPackage,
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
            package = if native then kajibaRunnerPackage else inputs.self.packages.${linux}.kajiba-runner;
            name = "build-1";
            url = "https://forge.example.org";
            tls = {
              trustDomain = "example.org";
              serverId = "spiffe://example.org/forge/kajiba-server";
              svidFile = "/var/lib/svid/kajiba-runner/svid.pem";
              keyFile = "/var/lib/svid/kajiba-runner/key.pem";
              bundleFile = "/var/lib/svid/kajiba-runner/bundle.pem";
            };
          };
          services.kajiba-server = {
            enable = true;
            package = if native then kajibaServerPackage else inputs.self.packages.${linux}.kajiba-server;
            ui.package = if native then kajibaUiPackage else inputs.self.packages.${linux}.kajiba-ui;
            http.publicUrl = lib.mkDefault "https://forge.example.org";
          };
        }
      ]
      ++ extras;
    };
  full = {
    services.kajiba-runner = {
      hostname = "vm-host.example.org";
      backends = [ "nix" ];
      tags = [ "kvm" ];
      maxJobs = 2;
      stateDir = "/srv/kajiba-runner";
      log.level = "debug";
      metrics.listen = "127.0.0.1:9432";
    };
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
      runnerListener = {
        listen = "0.0.0.0:8443";
        trustDomain = "example.org";
        svidFile = "/var/lib/svid/kajiba-server/svid.pem";
        keyFile = "/var/lib/svid/kajiba-server/key.pem";
        bundleFile = "/var/lib/svid/kajiba-server/bundle.pem";
      };
      runners.build-1 = {
        spiffeId = "spiffe://example.org/build-1/kajiba-runner";
        capabilities = {
          arch = "aarch64";
          os = "linux";
          tags = [ "kvm" ];
          maxJobs = 2;
        };
      };
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
        ci = "push";
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
        runAt = "04:30";
        worktreeRetention = "2h";
      };
      dump = {
        dir = "/srv/kajiba-dumps";
        keep = 3;
      };
    };
  };
  runnerFileOf = evaluation: evaluation.config.environment.etc."kajiba/runner.toml".source;
  expectedRunner = (pkgs.formats.toml { }).generate "expected-kajiba-runner.toml" {
    name = "build-1";
    url = "https://forge.example.org";
    tls = {
      trust_domain = "example.org";
      server_id = "spiffe://example.org/forge/kajiba-server";
      svid_file = "/var/lib/svid/kajiba-runner/svid.pem";
      key_file = "/var/lib/svid/kajiba-runner/key.pem";
      bundle_file = "/var/lib/svid/kajiba-runner/bundle.pem";
    };
    hostname = "vm-host.example.org";
    backends = [ "nix" ];
    tags = [ "kvm" ];
    max_jobs = 2;
    state_dir = "/srv/kajiba-runner";
    log.level = "debug";
    metrics.listen = "127.0.0.1:9432";
  };
  # nix-darwin is not an input: evaluate darwinModules.runner against the few options it sets.
  darwin = lib.evalModules {
    specialArgs = { inherit pkgs; };
    modules = [
      ../darwinModules/runner.nix
      {
        options = {
          launchd.daemons = lib.mkOption { type = lib.types.attrsOf lib.types.anything; };
          environment.etc = lib.mkOption { type = lib.types.attrsOf lib.types.anything; };
          nix.package = lib.mkOption {
            type = lib.types.package;
            default = pkgs.nix;
          };
        };
        config.services.kajiba-runner = {
          enable = true;
          package = kajibaRunnerPackage;
          name = "build-1";
          url = "https://forge.example.org";
          tls = {
            trustDomain = "example.org";
            serverId = "spiffe://example.org/forge/kajiba-server";
            svidFile = "/var/lib/svid/kajiba-runner/svid.pem";
            keyFile = "/var/lib/svid/kajiba-runner/key.pem";
            bundleFile = "/var/lib/svid/kajiba-runner/bundle.pem";
          };
        };
      }
    ];
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
    runner_listener = {
      listen = "0.0.0.0:8443";
      trust_domain = "example.org";
      svid_file = "/var/lib/svid/kajiba-server/svid.pem";
      key_file = "/var/lib/svid/kajiba-server/key.pem";
      bundle_file = "/var/lib/svid/kajiba-server/bundle.pem";
    };
    runners.build-1 = {
      spiffe_id = "spiffe://example.org/build-1/kajiba-runner";
      capabilities = {
        arch = "aarch64";
        os = "linux";
        tags = [ "kvm" ];
        max_jobs = 2;
      };
    };
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
      ci = "push";
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
      run_at = "04:30";
      worktree_retention = "2h";
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
    # 0133: every CI mode evaluates, a typo does not, and the removed boolean names its replacement.
    assert lib.all (ci: assertionsPass { services.kajiba-server.repos.tools = { inherit ci; }; }) [
      "off"
      "manual"
      "push"
    ];
    assert evaluationFails { services.kajiba-server.repos.tools.ci = "on"; };
    assert lib.any (a: !a.assertion && lib.hasInfix ''ci = "push" for true'' a.message)
      (evaluate false { services.kajiba-server.repos.tools.ciEnabled = true; }).config.assertions;
    assert lib.all
      (
        mode:
        assertionsPass {
          systemd.tmpfiles.settings."10-kajiba-secret"."/var/lib/svid/kajiba-server/key.pem".f = {
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
          systemd.tmpfiles.settings."10-kajiba-secret"."/var/lib/svid/kajiba-server/key.pem".f = secret;
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
        systemd.tmpfiles.settings."10-kajiba-secret"."/var/lib/svid/kajiba-server/key.pem".f = {
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
    assert lib.all
      (
        group:
        !(assertionsPass {
          users.groups.shared-secret.gid = 4242;
          systemd.services.kajiba-git.serviceConfig.SupplementaryGroups = [ group ];
          systemd.tmpfiles.settings."10-kajiba-secret"."/var/lib/svid/kajiba-server/key.pem".f = {
            mode = "0640";
            group = "shared-secret";
          };
        })
      )
      [
        "shared-secret"
        "4242"
      ];
    assert
      (evaluate false {
        users.groups.shared-secret.gid = 4242;
        systemd.services.kajiba-git.serviceConfig.SupplementaryGroups = [ "4242" ];
      }).config.services.kajiba-server.git.groupIds == [ 4242 ];
    assert
      !(assertionsPass {
        services.kajiba-server.runnerListener.keyFile = lib.mkForce "/etc/kajiba-secret";
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
          "tcp:8443"
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
      # The file reloads the unit; the package and the settings outside users, runners,
      # repos and mirror restart it (0141).
      && systemd.services.kajiba-server.reloadTriggers == [ environment.etc."kajiba/server.toml".source ]
      &&
        lib.head systemd.services.kajiba-server.restartTriggers
        == inputs.self.packages.${linux}.kajiba-server
      && lib.hasInfix " serve --config /etc/kajiba/server.toml" unit.ExecStart
      && lib.hasSuffix "kill -HUP $MAINPID" unit.ExecReload
      && lib.elem inputs.self.packages.${linux}.kajiba-server environment.systemPackages;
    # Declaring a user changes only the reload triggers; a listen address restarts.
    assert
      let
        triggers =
          modules:
          lib.getAttrs [
            "restartTriggers"
            "reloadTriggers"
          ] (evaluateWith false modules).config.systemd.services.kajiba-server;
        base = triggers [ ];
        user = triggers [ { services.kajiba-server.users.zed = { }; } ];
        moved = triggers [ { services.kajiba-server.http.listen = "127.0.0.1:8081"; } ];
      in
      user.restartTriggers == base.restartTriggers
      && user.reloadTriggers != base.reloadTriggers
      && moved.restartTriggers != base.restartTriggers;
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
        && unit.KillMode == "mixed"
        && unit.TimeoutStopSec == "35s"
        && unit.RuntimeDirectoryPreserve == "yes"
        && git.restartIfChanged
        && !git.reloadIfChanged
        && server.restartIfChanged
        && !server.reloadIfChanged
        && lib.elem "kajiba-git.socket" git.requires
        && lib.elem "kajiba-git-ssh.socket" git.after
        && lib.elem "kajiba-server.socket" server.requires
        && lib.elem "kajiba-server.socket" server.after
        &&
          systemd.sockets.kajiba-server.listenStreams == [
            (
              if services.kajiba-server.http.listen == null then
                "127.0.0.1:8080"
              else
                services.kajiba-server.http.listen
            )
          ]
        && (
          services.kajiba-server.runnerListener == null
          || (
            systemd.sockets.kajiba-server-runner.listenStreams
            == [ services.kajiba-server.runnerListener.listen ]
            && lib.elem "kajiba-server-runner.socket" server.requires
            && systemd.sockets.kajiba-server-runner.socketConfig.FileDescriptorName == "runner"
          )
        )
        && systemd.sockets.kajiba-server.socketConfig.FileDescriptorName == "http"
        && systemd.sockets.kajiba-git.socketConfig.SocketMode == "0660"
        && systemd.sockets.kajiba-git-ssh.socketConfig.SocketMode == "0600"
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
    # Runner unit (0009, 0015, 0022): fixed identity outside trusted-users, nix and git on PATH.
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
          runner = systemd.services.kajiba-runner;
          unit = runner.serviceConfig;
          stateDir = services.kajiba-runner.stateDir;
          configFile = environment.etc."kajiba/runner.toml".source;
        in
        users.users.kajiba-runner.isSystemUser
        && users.users.kajiba-runner.group == "kajiba-runner"
        && users.groups ? kajiba-runner
        && !(lib.elem "kajiba-runner" evaluation.config.nix.settings.trusted-users)
        && !(unit ? DynamicUser)
        && !(unit ? PrivateUsers)
        && unit.User == "kajiba-runner"
        && unit.Group == "kajiba-runner"
        && unit.ExecStart == "${lib.getExe services.kajiba-runner.package} --config ${configFile}"
        &&
          runner.restartTriggers == [
            configFile
            services.kajiba-runner.package
          ]
        && runner.environment.HOME == stateDir
        && lib.all (package: lib.elem package runner.path) [
          evaluation.config.nix.package
          evaluation.pkgs.git
          evaluation.pkgs.coreutils
        ]
        && unit.ReadWritePaths == [ stateDir ]
        && unit.StateDirectoryMode == "0700"
        && unit.UMask == "0077"
        && unit.ProtectSystem == "strict"
        && unit.ProtectHome
        && unit.NoNewPrivileges
        && unit.CapabilityBoundingSet == ""
        && lib.elem "AF_UNIX" unit.RestrictAddressFamilies
        && unit.SocketBindDeny == "any"
        && lib.elem "multi-user.target" runner.wantedBy
      )
      [
        (evaluate false { })
        (evaluateWith false [ ])
      ];
    assert
      let
        full = (evaluate false { }).config;
        defaults = (evaluateWith false [ ]).config;
      in
      full.systemd.services.kajiba-runner.serviceConfig.StateDirectory == [ ]
      && full.systemd.tmpfiles.settings."10-kajiba-runner"."/srv/kajiba-runner".d.user == "kajiba-runner"
      && full.systemd.services.kajiba-runner.serviceConfig.SocketBindAllow == [ "tcp:9432" ]
      && defaults.systemd.services.kajiba-runner.serviceConfig.StateDirectory == [ "kajiba-runner" ]
      && defaults.systemd.tmpfiles.settings."10-kajiba-runner" == { }
      && defaults.systemd.services.kajiba-runner.serviceConfig.SocketBindAllow == [ ];
    assert !(assertionsPass { nix.settings.trusted-users = [ "@kajiba-runner" ]; });
    assert !(assertionsPass { services.kajiba-runner.metrics.listen = lib.mkForce "0.0.0.0:9432"; });
    assert
      !(builtins.tryEval (
        builtins.deepSeq
          (runnerFileOf (
            evaluate true {
              services.kajiba-runner.tls.keyFile = lib.mkForce "${builtins.storeDir}/x";
            }
          )).drvPath
          true
      )).success;
    # darwinModules.runner: the same options, as a launchd daemon.
    assert
      let
        daemon = darwin.config.launchd.daemons.kajiba-runner.serviceConfig;
      in
      daemon.ProgramArguments == [
        (lib.getExe kajibaRunnerPackage)
        "--config"
        "${runnerFileOf darwin}"
      ]
      && daemon.UserName == "kajiba-runner"
      && daemon.GroupName == "kajiba-runner"
      && daemon.KeepAlive
      && daemon.EnvironmentVariables.HOME == "/var/lib/kajiba-runner"
      && lib.hasInfix (builtins.unsafeDiscardStringContext "${pkgs.git}/bin") daemon.EnvironmentVariables.PATH;
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
    # The bundle is served by default (0128), and the flag is gone once disabled.
    assert
      (evaluate false { }).config.systemd.services.kajiba-server.serviceConfig.ExecStart == "${
        lib.getExe inputs.self.packages.${linux}.kajiba-server
      } serve --config /etc/kajiba/server.toml --ui-dir ${inputs.self.packages.${linux}.kajiba-ui}";
    assert
      (evaluate false { services.kajiba-server.ui.enable = false; })
      .config.systemd.services.kajiba-server.serviceConfig.ExecStart == "${
        lib.getExe inputs.self.packages.${linux}.kajiba-server
      } serve --config /etc/kajiba/server.toml";
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
    # Capabilities are required (0107): no default system.
    assert evaluationFails {
      services.kajiba-server.runners.build-2.spiffeId = "spiffe://example.org/build-2/kajiba-runner";
    };
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
      (keyFile: evaluationFails { services.kajiba-server.runnerListener.keyFile = lib.mkForce keyFile; })
      [
        "${builtins.storeDir}/00000000000000000000000000000000-secret"
        "${pkgs.writeText "kajiba-secret" "DO-NOT-PRINT"}"
      ];
    assert
      !evaluationFails {
        services.kajiba-server.runnerListener.keyFile = lib.mkForce "/run/secrets/nix-store-lookalike";
      };
    pkgs.runCommand "kajiba-module-eval" { } ''
      cmp ${configFile { }} ${expected}
      cmp ${runnerFileOf (evaluate true { })} ${expectedRunner}
      # The default runner file holds only the required fields: name, url and the five of tls.
      [ "$(grep -c = ${runnerFileOf (evaluateWith true [ ])})" = 7 ]
      cmp ${runnerFileOf darwin} ${runnerFileOf (evaluateWith true [ ])}
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
  module-runner-listener = refused "runner_listener: required to declare runners.build-1" {
    services.kajiba-server.runnerListener = lib.mkForce null;
  };
  module-runner-id = refused "runners.build-1.spiffe_id" {
    services.kajiba-server.runners.build-1.spiffeId =
      lib.mkForce "spiffe://example.com/build-1/kajiba-runner";
  };
  module-runner-os = refused "runners.build-1.capabilities.os" {
    services.kajiba-server.runners.build-1.capabilities.os = lib.mkForce "macos";
  };
}
