# Packages, devShell, checks and formatter for every system (0015).
{ inputs, ... }:
{
  perSystem =
    {
      config,
      pkgs,
      lib,
      system,
      ...
    }:
    let
      craneLib = inputs.crane.mkLib pkgs;
      builds = import ./packages {
        inherit pkgs lib craneLib;
      };
      inherit (builds) commonCargoArgs cargoArtifacts;
    in
    {
      packages = builds.packages;

      apps.e2e = {
        type = "app";
        program = lib.getExe (
          pkgs.callPackage ./apps/e2e.nix {
            kajibaServer = config.packages.kajiba-server;
            kajibaGit = config.packages.kajiba-git;
            kajibaRunner = config.packages.kajiba-runner;
            kajibaCli = config.packages.kajiba-cli;
            kajibaUi = config.packages.kajiba-ui;
            svidCa = (import ./tests/svid.nix { inherit pkgs; }).ca;
          }
        );
        meta.description = "Playwright e2e suite against both daemons (CI job e2e)";
      };

      devShells.default = pkgs.mkShellNoCC {
        packages = with pkgs; [
          cargo
          rustc
          rust-analyzer
          clippy
          nodejs
          pnpm
          buf
          protobuf
          sqlx-cli
          mold
          pkg-config
          openssl
          playwright
        ];
        PROTOC = "${pkgs.protobuf}/bin/protoc";
        shellHook = ''
          export PATH="$PWD/apps/ui/node_modules/.bin:$PATH"
        '';
      };

      checks = {
        inherit (config.packages)
          kajiba-server
          kajiba-git
          kajiba-runner
          kajiba-cli
          kajiba-ui
          ;

        package-source-isolation = import ./checks/package-source-isolation.nix {
          inherit pkgs lib craneLib;
          inherit (builds) packages cargoArtifacts;
          workspaceSrc = commonCargoArgs.src;
        };

        cargo-test = craneLib.cargoTest (
          commonCargoArgs
          // {
            inherit cargoArtifacts;
            # Tests build Git fixtures and inspect HTTP child cleanup with ps.
            nativeBuildInputs = commonCargoArgs.nativeBuildInputs ++ [
              pkgs.git
              pkgs.openssh
              pkgs.procps
            ];
          }
        );
        cargo-clippy = craneLib.cargoClippy (
          commonCargoArgs
          // {
            inherit cargoArtifacts;
            cargoClippyExtraArgs = "--all-targets -- --deny warnings";
          }
        );

        versions-identical =
          pkgs.runCommand "versions-identical"
            {
              nativeBuildInputs = [ pkgs.nushell ];
              src = lib.fileset.toSource {
                root = ../.;
                fileset = lib.fileset.unions [
                  ../VERSION
                  ../Cargo.toml
                  ../apps/git/Cargo.toml
                  ../apps/ui/package.json
                ];
              };
            }
            ''
              cd $src
              nu ${./checks/versions-identical.nu}
              touch $out
            '';

        product-wording =
          pkgs.runCommand "product-wording"
            {
              src = lib.fileset.toSource {
                root = ../.;
                fileset = lib.fileset.difference ../. ../docs;
              };
            }
            ''
              bash ${./checks/product-wording.sh} "$src"
              bash ${./checks/product-wording-test.sh} ${./checks/product-wording.sh}
              touch $out
            '';

        # Gzip size of the initial JS, CSS and fonts, 300 KB (J5 task 9).
        ui-budget = pkgs.runCommand "ui-budget" { } ''
          bash ${./checks/ui-budget-test.sh} ${./checks/ui-budget.sh}
          bash ${./checks/ui-budget.sh} ${config.packages.kajiba-ui} 300000
          touch $out
        '';

        buf-lint = pkgs.runCommand "buf-lint" { nativeBuildInputs = [ pkgs.buf ]; } ''
          export HOME=$TMPDIR
          cd ${../proto}
          buf lint
          touch $out
        '';

        proto-generated = pkgs.stdenvNoCC.mkDerivation {
          pname = "proto-generated";
          inherit (config.packages.kajiba-ui) version pnpmDeps;
          src = lib.fileset.toSource {
            root = ../.;
            fileset = lib.fileset.unions [
              (lib.fileset.fromSource config.packages.kajiba-ui.src)
              ../proto
            ];
          };
          nativeBuildInputs = with pkgs; [
            nodejs
            pnpm
            pnpmConfigHook
            buf
            diffutils
          ];
          buildPhase = ''
            runHook preBuild
            export HOME=$TMPDIR
            export PATH="$PWD/apps/ui/node_modules/.bin:$PATH"
            bash ${./checks/proto-generated.sh}
            bash ${./checks/proto-generated-test.sh} ${./checks/proto-generated.sh}
            runHook postBuild
          '';
          installPhase = "touch $out";
        };

        # Against the last release's proto/, the `proto-baseline` flake input once a release
        # exists (ADR 0101); informational before 1.0 (0020).
        buf-breaking = pkgs.runCommand "buf-breaking" { nativeBuildInputs = [ pkgs.buf ]; } ''
          export HOME=$TMPDIR
          bash ${./checks/buf-breaking-test.sh} ${./checks/buf-breaking.sh} ${../proto}
          bash ${./checks/buf-breaking.sh} ${../proto} '${
            lib.optionalString (inputs ? proto-baseline) "${inputs.proto-baseline}/proto"
          }' ${commonCargoArgs.version}
          touch $out
        '';
      }
      // import ./checks/module.nix {
        inherit inputs pkgs system;
        kajibaServerPackage = config.packages.kajiba-server;
        kajibaGitPackage = config.packages.kajiba-git;
        kajibaRunnerPackage = config.packages.kajiba-runner;
        kajibaUiPackage = config.packages.kajiba-ui;
      }
      // lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux (
        let
          testLib = import ./tests/lib.nix {
            inherit pkgs;
            inherit (inputs) nisshi masu;
          };
        in
        {
          nixos-cli = import ./tests/cli.nix {
            inherit pkgs testLib;
            kajibaServerPackage = config.packages.kajiba-server;
            kajibaGitPackage = config.packages.kajiba-git;
            kajibaCliPackage = config.packages.kajiba-cli;
          };
          nixos-ci-admission = import ./tests/ci-admission.nix {
            inherit pkgs testLib;
            kajibaServerPackage = config.packages.kajiba-server;
            kajibaGitPackage = config.packages.kajiba-git;
            kajibaRunnerPackage = config.packages.kajiba-runner;
            kajibaCliPackage = config.packages.kajiba-cli;
          };
          nixos-ci-pipeline = import ./tests/ci-pipeline.nix {
            inherit pkgs testLib;
            kajibaServerPackage = config.packages.kajiba-server;
            kajibaGitPackage = config.packages.kajiba-git;
            kajibaRunnerPackage = config.packages.kajiba-runner;
            kajibaCliPackage = config.packages.kajiba-cli;
          };
          nixos-ops-dump = import ./tests/ops-dump.nix {
            inherit pkgs testLib;
            kajibaServerPackage = config.packages.kajiba-server;
            kajibaGitPackage = config.packages.kajiba-git;
            kajibaRunnerPackage = config.packages.kajiba-runner;
            kajibaCliPackage = config.packages.kajiba-cli;
          };
          nixos-ops-gc = import ./tests/ops-gc.nix {
            inherit pkgs testLib;
            kajibaServerPackage = config.packages.kajiba-server;
            kajibaGitPackage = config.packages.kajiba-git;
            kajibaRunnerPackage = config.packages.kajiba-runner;
            kajibaCliPackage = config.packages.kajiba-cli;
          };
          nixos-ops-timers = import ./tests/ops-timers.nix {
            inherit pkgs testLib;
            kajibaServerPackage = config.packages.kajiba-server;
            kajibaGitPackage = config.packages.kajiba-git;
            kajibaRunnerPackage = config.packages.kajiba-runner;
            kajibaCliPackage = config.packages.kajiba-cli;
          };
          nixos-ops = import ./tests/ops.nix {
            inherit pkgs testLib;
            kajibaServerPackage = config.packages.kajiba-server;
            kajibaGitPackage = config.packages.kajiba-git;
            kajibaRunnerPackage = config.packages.kajiba-runner;
            kajibaCliPackage = config.packages.kajiba-cli;
          };
          nixos-ops-restore = import ./tests/ops-restore.nix {
            inherit pkgs testLib;
            kajibaServerPackage = config.packages.kajiba-server;
            kajibaGitPackage = config.packages.kajiba-git;
            kajibaRunnerPackage = config.packages.kajiba-runner;
            kajibaCliPackage = config.packages.kajiba-cli;
          };
          nixos-ci-recovery = import ./tests/ci-recovery.nix {
            inherit pkgs testLib;
            kajibaServerPackage = config.packages.kajiba-server;
            kajibaGitPackage = config.packages.kajiba-git;
            kajibaRunnerPackage = config.packages.kajiba-runner;
            kajibaCliPackage = config.packages.kajiba-cli;
          };
          nixos-ci-worktree = import ./tests/ci-worktree.nix {
            inherit pkgs testLib;
            kajibaServerPackage = config.packages.kajiba-server;
            kajibaGitPackage = config.packages.kajiba-git;
            kajibaRunnerPackage = config.packages.kajiba-runner;
            kajibaCliPackage = config.packages.kajiba-cli;
          };
          nixos-server-lifecycle = import ./tests/server-lifecycle.nix {
            inherit pkgs testLib;
            kajibaServerPackage = config.packages.kajiba-server;
            kajibaGitPackage = config.packages.kajiba-git;
            kajibaUiPackage = config.packages.kajiba-ui;
          };
          nixos-server-config = import ./tests/server-config.nix {
            inherit pkgs testLib;
            kajibaServerPackage = config.packages.kajiba-server;
            kajibaGitPackage = config.packages.kajiba-git;
          };
          nixos-server-observability = import ./tests/server-observability.nix {
            inherit pkgs testLib;
            kajibaServerPackage = config.packages.kajiba-server;
            kajibaGitPackage = config.packages.kajiba-git;
          };
          nixos-server-enroll = import ./tests/server-enroll.nix {
            inherit pkgs testLib;
            kajibaServerPackage = config.packages.kajiba-server;
            kajibaGitPackage = config.packages.kajiba-git;
          };
          nixos-ssh-transport = import ./tests/ssh-transport.nix {
            inherit pkgs testLib;
            kajibaServerPackage = config.packages.kajiba-server;
            kajibaGitPackage = config.packages.kajiba-git;
          };
          nixos-git-ssh = import ./tests/git-ssh.nix {
            inherit pkgs testLib;
            kajibaServerPackage = config.packages.kajiba-server;
            kajibaGitPackage = config.packages.kajiba-git;
          };
          nixos-git-ssh-recovery = import ./tests/git-ssh-recovery.nix {
            inherit pkgs testLib;
            kajibaServerPackage = config.packages.kajiba-server;
            kajibaGitPackage = config.packages.kajiba-git;
          };
          nixos-git-http = import ./tests/git-http.nix {
            inherit pkgs testLib;
            kajibaServerPackage = config.packages.kajiba-server;
            kajibaGitPackage = config.packages.kajiba-git;
          };
          nixos-import-recovery = import ./tests/import-recovery.nix {
            inherit pkgs testLib;
            kajibaServerPackage = config.packages.kajiba-server;
            kajibaGitPackage = config.packages.kajiba-git;
          };
          nixos-repo-lifecycle = import ./tests/repo-lifecycle.nix {
            inherit pkgs testLib;
            kajibaServerPackage = config.packages.kajiba-server;
            kajibaGitPackage = config.packages.kajiba-git;
          };
        }
      );

      treefmt = {
        projectRootFile = "flake.nix";
        programs = {
          rustfmt.enable = true;
          prettier.enable = true;
          nixfmt.enable = true;
        };
        settings.excludes = [
          "apps/ui/src/lib/gen/**"
          "apps/ui/build/**"
          "apps/ui/.svelte-kit/**"
          "node_modules/**"
          "target/**"
          "Cargo.lock"
          "pnpm-lock.yaml"
        ];
      };
    };
}
