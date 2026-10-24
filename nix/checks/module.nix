# Server module checks (0015, 0019, 0022), after oroshi's: a full-option configuration
# evaluates, its generated file passes `config check` and matches the expected TOML; invalid
# options are refused at evaluation or at build time.
{
  inputs,
  pkgs,
  system,
  kajibaServerPackage,
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
        {
          nixpkgs.hostPlatform = if native then system else linux;
          boot.loader.grub.enable = false;
          fileSystems."/" = {
            device = "none";
            fsType = "tmpfs";
          };
          system.stateVersion = "26.05";
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
    services.kajiba-server = {
      stateDir = "/srv/kajiba";
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
    state_dir = "/srv/kajiba";
    http = {
      listen = "127.0.0.1:8081";
      public_url = "https://forge.example.org";
      reflection = true;
    };
    ssh = {
      host = "ssh.example.org";
      port = 22;
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
    # Generated unit, identity and persisted paths (0012, 0015, 0022), full then default options.
    assert
      let
        inherit ((evaluate false { }).config) systemd users services;
        unit = systemd.services.kajiba-server.serviceConfig;
      in
      !(unit ? DynamicUser)
      && unit.User == "kajiba"
      && unit.Group == "kajiba"
      && unit.StateDirectory == [ ]
      &&
        unit.ReadWritePaths == [
          "/srv/kajiba"
          "/srv/kajiba-dumps"
        ]
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
      && users.users.kajiba.isSystemUser
      && users.users.kajiba.group == "kajiba"
      && users.groups ? kajiba
      && systemd.tmpfiles.settings."10-kajiba-server" ? "/srv/kajiba"
      && systemd.tmpfiles.settings."10-kajiba-server"."/srv/kajiba-dumps".d.user == "kajiba"
      &&
        services.kajiba-server.persistedPaths == [
          "/srv/kajiba/repos"
          "/srv/kajiba/db"
          "/srv/kajiba/ci"
          "/srv/kajiba-dumps"
        ];
    assert
      let
        inherit ((evaluateWith false [ ]).config) systemd environment;
        unit = systemd.services.kajiba-server.serviceConfig;
      in
      unit.StateDirectory == [
        "kajiba"
        "kajiba-dumps"
      ]
      && systemd.tmpfiles.settings."10-kajiba-server" == { }
      && unit.SocketBindAllow == [ "tcp:8080" ]
      && lib.hasSuffix " http://127.0.0.1:8080/readyz" unit.ExecStartPost
      && systemd.services.kajiba-server.restartTriggers == [ environment.etc."kajiba/server.toml".source ]
      && lib.elem inputs.self.packages.${linux}.kajiba-server environment.systemPackages;
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
    assert evaluationFails { services.kajiba-server.unexpected = true; };
    assert evaluationFails { services.kajiba-server.runners.build-1.token = "inline-secret"; };
    assert evaluationFails {
      services.kajiba-server.mirror.credentials.github.tokenFile = lib.mkForce ./module.nix;
    };
    pkgs.runCommand "kajiba-module-eval" { } ''
      cmp ${configFile { }} ${expected}
      # Defaults stay in the code (0019): only the required public URL is written.
      [ "$(grep -c = ${configFileOf (evaluateWith true [ ])})" = 1 ]
      echo ${
        builtins.unsafeDiscardStringContext (evaluate false { }).config.system.build.toplevel.drvPath
      }
      touch "$out"
    '';

  # Generated file refused by `config check` although the option types accept it.
  module-metrics-listen = refused "metrics.listen: must be a loopback address" {
    services.kajiba-server.metrics.listen = lib.mkForce "0.0.0.0:9431";
  };
  module-token-hash = refused "users.alice.tokens.ci.sha256" {
    services.kajiba-server.users.alice.tokens.ci.sha256 = lib.mkForce "not-a-hash";
  };
}
