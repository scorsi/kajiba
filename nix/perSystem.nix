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
      commonCargoArgs = {
        pname = "kajiba-workspace";
        version = lib.trim (builtins.readFile ../VERSION);
        src = lib.fileset.toSource {
          root = ../.;
          fileset = lib.fileset.unions [
            (craneLib.fileset.commonCargoSources ../crates)
            ../crates/kajiba-core/tests/fixtures
            (craneLib.fileset.commonCargoSources ../apps/server)
            ../apps/server/migrations
            ../apps/server/tests/fixtures
            (craneLib.fileset.commonCargoSources ../apps/runner)
            (craneLib.fileset.commonCargoSources ../apps/cli)
            ../Cargo.toml
            ../VERSION
            ../Cargo.lock
            ../proto
          ];
        };
        strictDeps = true;
        nativeBuildInputs = [ pkgs.protobuf ];
        PROTOC = "${pkgs.protobuf}/bin/protoc";
        cargoExtraArgs = "--locked --workspace";
      };
      cargoArtifacts = craneLib.buildDepsOnly commonCargoArgs;
    in
    {
      packages = {
        kajiba-server = pkgs.callPackage ./packages/kajiba-server.nix {
          inherit craneLib commonCargoArgs cargoArtifacts;
        };
        kajiba-runner = pkgs.callPackage ./packages/kajiba-runner.nix {
          inherit craneLib commonCargoArgs cargoArtifacts;
        };
        kajiba-cli = pkgs.callPackage ./packages/kajiba-cli.nix {
          inherit craneLib commonCargoArgs cargoArtifacts;
        };
        kajiba-ui = pkgs.callPackage ./packages/kajiba-ui.nix { };
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
          kajiba-runner
          kajiba-cli
          kajiba-ui
          ;

        cargo-test = craneLib.cargoTest (commonCargoArgs // { inherit cargoArtifacts; });
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

        buf-breaking = pkgs.runCommand "buf-breaking" { } ''
          echo "buf-breaking: informational; always skipped in the Nix sandbox, whose source has no git history to compare against. Run it manually: see CONTRIBUTING.md."
          touch $out
        '';
      }
      // import ./checks/module.nix {
        inherit inputs pkgs system;
        kajibaServerPackage = config.packages.kajiba-server;
      }
      // lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux (
        let
          testLib = import ./tests/lib.nix {
            inherit pkgs;
            inherit (inputs) nisshi masu;
          };
        in
        {
          nixos-server-lifecycle = import ./tests/server-lifecycle.nix {
            inherit pkgs testLib;
            kajibaServerPackage = config.packages.kajiba-server;
          };
          nixos-server-config = import ./tests/server-config.nix {
            inherit pkgs testLib;
            kajibaServerPackage = config.packages.kajiba-server;
          };
          nixos-server-observability = import ./tests/server-observability.nix {
            inherit pkgs testLib;
            kajibaServerPackage = config.packages.kajiba-server;
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
