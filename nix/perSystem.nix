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
    in
    {
      packages = {
        kajiba-server = pkgs.callPackage ./packages/kajiba-server.nix { inherit craneLib; };
        kajiba-runner = pkgs.callPackage ./packages/kajiba-runner.nix { inherit craneLib; };
        kajiba-cli = pkgs.callPackage ./packages/kajiba-cli.nix { };
        kajiba-ui = pkgs.callPackage ./packages/kajiba-ui.nix { };
      };

      devShells.default = pkgs.mkShellNoCC {
        packages = with pkgs; [
          cargo
          rustc
          rust-analyzer
          clippy
          go
          nodejs
          pnpm
          buf
          protobuf
          protoc-gen-go
          protoc-gen-go-grpc
          sqlx-cli
          mold
          playwright
        ];
        PROTOC = "${pkgs.protobuf}/bin/protoc";
      };

      checks = {
        versions-identical =
          pkgs.runCommand "versions-identical"
            {
              nativeBuildInputs = [ pkgs.nushell ];
              src = lib.fileset.toSource {
                root = ../.;
                fileset = lib.fileset.unions [
                  ../VERSION
                  ../Cargo.toml
                  ../apps/cli/internal/version/version.go
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
          gofmt.enable = true;
          prettier.enable = true;
          nixfmt.enable = true;
        };
        settings.excludes = [
          "apps/cli/internal/genproto/**"
          "apps/ui/src/lib/gen/**"
          "apps/ui/build/**"
          "apps/ui/.svelte-kit/**"
          "node_modules/**"
          "target/**"
          "Cargo.lock"
          "pnpm-lock.yaml"
          "apps/cli/go.sum"
        ];
      };
    };
}
