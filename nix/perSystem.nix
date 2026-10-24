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
        src = lib.fileset.toSource {
          root = ../.;
          fileset = lib.fileset.unions [
            (craneLib.fileset.commonCargoSources ../crates)
            (craneLib.fileset.commonCargoSources ../apps/server)
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

        buf-lint = pkgs.runCommand "buf-lint" { nativeBuildInputs = [ pkgs.buf ]; } ''
          export HOME=$TMPDIR
          cd ${../proto}
          buf lint
          touch $out
        '';
      }
      // lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
        nixos-healthz = import ./tests/healthz.nix {
          inherit pkgs;
          kajibaServerPackage = config.packages.kajiba-server;
        };
      };

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
