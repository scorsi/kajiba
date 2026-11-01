# Evaluate filtered copies, without building them or recursively evaluating the flake.
{
  pkgs,
  lib,
  craneLib,
  packages,
  cargoArtifacts,
  workspaceSrc,
}:
let
  source = lib.fileset.toSource {
    root = ../..;
    fileset = lib.fileset.unions [
      (lib.fileset.fromSource workspaceSrc)
      (lib.fileset.fromSource packages.kajiba-ui.src)
    ];
  };
  rust = [
    "kajiba-server"
    "kajiba-git"
    "kajiba-runner"
    "kajiba-cli"
  ];
  probes = [
    {
      file = "apps/ui/src/lib/api.ts";
      affected = [ "kajiba-ui" ];
    }
    {
      file = "apps/server/src/metrics.rs";
      affected = [ "kajiba-server" ];
    }
    {
      file = "apps/git/src/metrics.rs";
      affected = [ "kajiba-git" ];
    }
    {
      file = "apps/runner/src/metrics.rs";
      affected = [ "kajiba-runner" ];
    }
    {
      file = "apps/cli/src/client.rs";
      affected = [ "kajiba-cli" ];
    }
    {
      file = "crates/kajiba-exec/src/tests.rs";
      affected = [ "kajiba-runner" ];
    }
    {
      file = "apps/server/build.rs";
      affected = [ "kajiba-server" ];
    }
    {
      file = "crates/kajiba-proto/build.rs";
      affected = rust;
    }
    {
      file = "apps/server/migrations/1_initial.sql";
      affected = [ "kajiba-server" ];
    }
    {
      file = "apps/ui/src/lib/gen/kajiba/v1/health_pb.ts";
      affected = [ "kajiba-ui" ];
    }
    {
      file = "crates/kajiba-core/tests/fixtures/runner.toml";
      affected = rust;
    }
    {
      file = "proto/kajiba/v1/health.proto";
      affected = rust;
    }
  ];
  check =
    { file, affected }:
    let
      # Removing a non-target source file changes its owner's drvPath, but must
      # leave every unrelated package (including shared cargo artifacts) alone.
      # fileset needs a path value; path concatenation rejects store string context.
      root =
        /.
        + builtins.unsafeDiscardStringContext (
          builtins.path {
            path = source;
            name = "source";
            filter = path: type: path != "${source}/${file}";
          }
        );
      changed = import ../packages {
        inherit
          pkgs
          lib
          craneLib
          root
          ;
      };
    in
    assert lib.assertMsg (builtins.pathExists (source + "/${file}")) "missing isolation probe: ${file}";
    assert lib.assertMsg (
      cargoArtifacts.drvPath == changed.cargoArtifacts.drvPath
    ) "${file}: shared cargo artifacts changed";
    lib.all (
      name:
      let
        differs = packages.${name}.drvPath != changed.packages.${name}.drvPath;
      in
      lib.assertMsg (differs == lib.elem name affected) "${file}: unexpected drvPath for ${name}"
    ) (builtins.attrNames packages);
in
assert lib.all check probes;
pkgs.runCommand "package-source-isolation" { } "touch $out"
